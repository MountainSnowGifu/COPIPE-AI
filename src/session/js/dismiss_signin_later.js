(function () {
  const labels = [
    "\u5f8c\u3067",
    "\u5f8c\u3067\u884c\u3046",
    "\u4eca\u306f\u3057\u306a\u3044",
    "Later",
    "Maybe later",
    "Not now",
    "Skip for now",
  ];
  const providerLabels = [
    "Microsoft \u3067\u7d9a\u884c",
    "Apple \u3067\u7d9a\u884c",
    "Google \u3067\u7d9a\u884c",
  ];
  const authHints = [
    "\u30b5\u30a4\u30f3\u30a4\u30f3",
    "\u30ed\u30b0\u30a4\u30f3",
    "\u7d9a\u884c",
    "\u30c1\u30e3\u30c3\u30c8\u3092\u4fdd\u6301",
    "\u5b8c\u5168\u306a\u30a8\u30af\u30b9\u30da\u30ea\u30a8\u30f3\u30b9",
    "Copilot Voice",
  ];

  const visible = (el) => {
    if (!el) return false;
    const style = window.getComputedStyle(el);
    if (style.visibility === "hidden" || style.display === "none") return false;
    const r = el.getBoundingClientRect();
    return r.width > 4 && r.height > 4;
  };

  const textOf = (el) =>
    [
      el?.innerText,
      el?.textContent,
      el?.getAttribute?.("aria-label"),
      el?.getAttribute?.("title"),
      el?.getAttribute?.("value"),
    ]
      .filter(Boolean)
      .join(" ")
      .replace(/\s+/g, " ")
      .trim();
  const compact = (s) => String(s || "").replace(/\s+/g, "");
  const labelMatches = (text, interactive) =>
    labels.some((label) => {
      const compactText = compact(text);
      const compactLabel = compact(label);
      return (
        text === label ||
        compactText === compactLabel ||
        (interactive && (text.includes(label) || compactText.includes(compactLabel)))
      );
    });
  const isInteractive = (el) =>
    el.matches(
      'button, [role="button"], a, input[type="button"], input[type="submit"], [tabindex]:not([tabindex="-1"])',
    );
  const interactiveFor = (el) =>
    el.closest(
      'button, [role="button"], a, input[type="button"], input[type="submit"], [tabindex]:not([tabindex="-1"])',
    ) || el;
  const collectElements = (root) => {
    const selectors =
      'button, [role="button"], a, input[type="button"], input[type="submit"], [tabindex]:not([tabindex="-1"]), div, span';
    const found = [];
    const visit = (node) => {
      try {
        found.push(...node.querySelectorAll(selectors));
        for (const el of node.querySelectorAll("*")) {
          if (el.shadowRoot) visit(el.shadowRoot);
        }
      } catch (_) {}
    };
    visit(root);
    return found;
  };

  const candidates = collectElements(document);

  let target = null;
  for (const el of candidates) {
    if (!visible(el)) continue;
    const text = textOf(el);
    const interactive = isInteractive(el);
    if (!labelMatches(text, interactive)) continue;
    if (!interactive && text.length > 40) continue;

    const modalText = textOf(el.closest('[role="dialog"], [aria-modal="true"], main, body'));
    const lowerModalText = modalText.toLowerCase();
    if (
      providerLabels.some((label) => modalText.includes(label)) ||
      (authHints.some((hint) => modalText.includes(hint)) &&
        ["Microsoft", "Apple", "Google"].some((provider) => modalText.includes(provider))) ||
      lowerModalText.includes("sign in") ||
      lowerModalText.includes("signin") ||
      lowerModalText.includes("log in")
    ) {
      target = interactiveFor(el);
      break;
    }
  }

  if (!target || !visible(target)) return null;

  target.scrollIntoView({ block: "center", inline: "center", behavior: "smooth" });
  const r = target.getBoundingClientRect();
  const tx = r.left + r.width * (0.45 + Math.random() * 0.1);
  const ty = r.top + r.height * (0.45 + Math.random() * 0.1);
  const topElement = document.elementFromPoint(tx, ty);
  const actualTarget = topElement ? interactiveFor(topElement) : target;
  const sx = Math.max(8, Math.min(window.innerWidth - 8, tx + (Math.random() - 0.5) * 180));
  const sy = Math.max(8, Math.min(window.innerHeight - 8, ty - 80 - Math.random() * 80));
  const steps = 9 + Math.floor(Math.random() * 7);

  for (let i = 0; i <= steps; i++) {
    const t = i / steps;
    const ease = t * t * (3 - 2 * t);
    const x = sx + (tx - sx) * ease + (Math.random() - 0.5) * 2;
    const y = sy + (ty - sy) * ease + (Math.random() - 0.5) * 2;
    actualTarget.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, clientX: x, clientY: y }));
  }

  const opts = {
    bubbles: true,
    cancelable: true,
    clientX: tx,
    clientY: ty,
    button: 0,
    buttons: 1,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
  };
  for (const ev of ["pointerover", "pointerenter", "pointerdown", "pointerup"]) {
    try {
      actualTarget.dispatchEvent(new PointerEvent(ev, opts));
    } catch (_) {}
  }
  for (const ev of ["mouseover", "mouseenter", "mousedown", "mouseup", "click"]) {
    actualTarget.dispatchEvent(new MouseEvent(ev, opts));
  }
  try {
    actualTarget.click();
  } catch (_) {}
  try {
    if (actualTarget !== target) target.click();
  } catch (_) {}

  return JSON.stringify({
    status: "clicked",
    text: textOf(actualTarget).slice(0, 80),
    x: tx,
    y: ty,
  });
})();
