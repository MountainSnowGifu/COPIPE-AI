(function () {
  // 優先セレクター（aria-label / data-testid）でボタンを探してクリックする
  const selectors = [
    'button[aria-label*="Send"]',
    'button[aria-label*="送信"]',
    'button[aria-label*="メッセージ"]',
    'button[aria-label*="message"]',
    'button[aria-label*="submit"]',
    'button[aria-label*="Submit"]',
    '[data-testid*="send"]',
    '[data-testid*="Send"]',
    '[data-testid*="submit"]',
    '[data-testid*="Submit"]',
    'button[type="submit"]',
  ];
  for (const sel of selectors) {
    const btn = document.querySelector(sel);
    if (btn && !btn.disabled) {
      btn.click();
      return "clicked:" + sel;
    }
  }

  // 入力欄の近くにある有効ボタンを最大5階層上まで探す
  const inp = window.__copipeFindInput
    ? window.__copipeFindInput()
    : document.querySelector(
        '#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]',
      );
  if (inp) {
    let el = inp.parentElement;
    for (let depth = 0; el && depth < 5; depth++, el = el.parentElement) {
      const btns = [...el.querySelectorAll("button:not([disabled])")];
      const inpRect = inp.getBoundingClientRect();
      for (const btn of btns) {
        const r = btn.getBoundingClientRect();
        if (r.left >= inpRect.right - 10 || r.top >= inpRect.bottom - 10) {
          btn.click();
          return (
            "clicked:nearby@" +
            depth +
            ":" +
            (btn.getAttribute("aria-label") ||
              btn.getAttribute("data-testid") ||
              btn.className.slice(0, 30) ||
              "unknown")
          );
        }
      }
    }
  }

  // 最終手段: form.requestSubmit()
  const form = inp?.closest("form");
  if (form) {
    try {
      form.requestSubmit();
      return "form_requestSubmit";
    } catch (_) {}
    try {
      form.submit();
      return "form_submit";
    } catch (_) {}
  }

  return null;
})();
