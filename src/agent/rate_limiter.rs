/// ターン間待機の動的制御（core-internals.md §4 rate-limiter.mjs を参考）
///
/// Claude Code の指数バックオフ + ジッターを COPIPE-AI 向けに適用。
///
/// 計算式（§4 準拠）:
///   exp    = base_ms × 2^consecutive_issues
///   jitter = rand(0 .. base_ms)
///   delay  = min(exp + jitter, max_ms)
///
/// 「問題なし」ターンが続けば consecutive_issues を徐々に減らし、
/// 通常の短い待機に戻る。

pub struct RateLimiter {
    /// 連続した問題ターン数（最大5）
    consecutive_issues: u32,
    /// 問題なし時の基本待機 ms
    base_ms: u64,
    /// 指数バックオフの上限 ms
    max_ms: u64,
    /// 人間らしい長停止の周期（N ターンに1回）
    human_pause_every: u32,
    /// 現在のターン番号（人間らしい長停止の判定に使用）
    turn: u32,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            consecutive_issues: 0,
            base_ms: 5_000,        // 5秒ベース（従来 2秒）
            max_ms: 20_000,        // 最大 20秒（従来 14秒）
            human_pause_every: 4,  // 4ターンに1回の長停止（従来 8ターン）
            turn: 0,
        }
    }

    /// ターンを1進める（呼び出し元で毎ターン呼ぶ）
    pub fn advance(&mut self) {
        self.turn += 1;
    }

    /// 応答が正常だったことを記録（consecutive_issues を徐々に回復）
    pub fn record_success(&mut self) {
        if self.consecutive_issues > 0 {
            self.consecutive_issues -= 1;
        }
    }

    /// 応答に問題があったことを記録（ParseError / 再要求 / 応答なし）
    pub fn record_issue(&mut self) {
        self.consecutive_issues = (self.consecutive_issues + 1).min(5);
    }

    /// 次のターン間待機時間 (ms) を計算する
    ///
    /// 指数バックオフ + ジッター + 人間らしい長停止の合計
    pub fn next_delay_ms(&self) -> u64 {
        let seed = seed_from_time();

        // 指数バックオフ: base × 2^issues + jitter
        let exp = self.base_ms.saturating_mul(1u64 << self.consecutive_issues);
        let jitter = seed % self.base_ms.max(1);
        let backoff = (exp + jitter).min(self.max_ms);

        // 人間らしい長停止: N ターンに1回 8〜18秒追加（従来 5〜10秒）
        let human_extra = if self.turn > 0
            && self.human_pause_every > 0
            && self.turn % self.human_pause_every as u32 == 0
        {
            8_000 + seed % 10_000 // 8〜18秒
        } else {
            0
        };

        (backoff + human_extra).min(self.max_ms + 10_000)
    }

    pub fn consecutive_issues(&self) -> u32 {
        self.consecutive_issues
    }
}

fn seed_from_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64
}

// ─── テスト ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_issues_stays_low() {
        let rl = RateLimiter::new();
        // 問題なしのとき delay は base + jitter 以内（長停止除く）
        // turn=0 は long pause 対象外
        let delay = rl.next_delay_ms();
        assert!(delay <= 14_000 + 10_000, "delay={delay} is too large");
    }

    #[test]
    fn test_exponential_backoff() {
        let mut rl = RateLimiter::new();
        // issues=0: base=2000, issues=1: 4000, issues=2: 8000
        assert_eq!(rl.consecutive_issues(), 0);

        rl.record_issue();
        assert_eq!(rl.consecutive_issues(), 1);
        let delay1 = rl.base_ms.saturating_mul(1u64 << 1); // 4000
        // next_delay_ms は jitter を含むが指数部分は正しいはず
        let d = rl.next_delay_ms();
        assert!(d >= delay1, "delay={d} should be >= {delay1}");

        rl.record_issue();
        assert_eq!(rl.consecutive_issues(), 2);
        let delay2 = rl.base_ms.saturating_mul(1u64 << 2); // 8000
        let d = rl.next_delay_ms();
        assert!(d >= delay2, "delay={d} should be >= {delay2}");
    }

    #[test]
    fn test_max_cap() {
        let mut rl = RateLimiter::new();
        for _ in 0..10 { rl.record_issue(); }
        assert_eq!(rl.consecutive_issues(), 5); // 5 でキャップ
        // max_ms 以内（長停止込みでも 24000ms 以内）
        let d = rl.next_delay_ms();
        assert!(d <= rl.max_ms + 10_000, "delay={d} exceeded cap");
    }

    #[test]
    fn test_recovery() {
        let mut rl = RateLimiter::new();
        rl.record_issue();
        rl.record_issue();
        rl.record_issue();
        assert_eq!(rl.consecutive_issues(), 3);

        rl.record_success();
        assert_eq!(rl.consecutive_issues(), 2);
        rl.record_success();
        assert_eq!(rl.consecutive_issues(), 1);
        rl.record_success();
        assert_eq!(rl.consecutive_issues(), 0);
        rl.record_success(); // 0 以下にはならない
        assert_eq!(rl.consecutive_issues(), 0);
    }

    #[test]
    fn test_human_pause_period() {
        let mut rl = RateLimiter { human_pause_every: 4, turn: 4, ..RateLimiter::new() };
        let d = rl.next_delay_ms();
        // turn=4 は long pause 対象なので base + jitter + 5000〜10000 ms
        assert!(d >= 5_000, "turn=4 should trigger human pause, delay={d}");
    }
}
