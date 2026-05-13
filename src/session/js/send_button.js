(function () {
  const isUsable = (el) => {
    if (!el) return false;
    const btn = el.closest("button, [role='button']") || el;
    const style = window.getComputedStyle(btn);
    const rect = btn.getBoundingClientRect();
    return (
      !btn.disabled &&
      btn.getAttribute("aria-disabled") !== "true" &&
      style.visibility !== "hidden" &&
      style.display !== "none" &&
      rect.width > 0 &&
      rect.height > 0
    );
  };

  const humanClick = (el) => {
    const target = el.closest("button, [role='button']") || el;
    target.scrollIntoView({ block: "center", inline: "center" });
    const r = target.getBoundingClientRect();
    const x = r.left + r.width / 2;
    const y = r.top + r.height / 2;
    const mouse = {
      bubbles: true,
      cancelable: true,
      composed: true,
      view: window,
      clientX: x,
      clientY: y,
      button: 0,
    };
    const pointer = { ...mouse, pointerId: 1, pointerType: "mouse", isPrimary: true };

    for (const ev of ["pointerover", "pointerenter", "pointerdown", "pointerup"]) {
      try {
        target.dispatchEvent(new PointerEvent(ev, pointer));
      } catch (_) {}
    }
    for (const ev of ["mouseover", "mouseenter", "mousedown", "mouseup", "click"]) {
      target.dispatchEvent(new MouseEvent(ev, mouse));
    }
    target.click();
    return target;
  };

  const selectors = [
    'button[aria-label*="Send"]',
    'button[aria-label*="send"]',
    'button[aria-label*="\u9001\u4fe1"]',
    'button[aria-label*="\u30e1\u30c3\u30bb\u30fc\u30b8"]',
    'button[aria-label*="message"]',
    'button[aria-label*="submit"]',
    'button[aria-label*="Submit"]',
    '[data-testid*="send"]',
    '[data-testid*="Send"]',
    '[data-testid*="submit"]',
    '[data-testid*="Submit"]',
    '[role="button"][aria-label*="Send"]',
    '[role="button"][aria-label*="send"]',
    '[role="button"][aria-label*="\u9001\u4fe1"]',
    '[role="button"][data-testid*="send"]',
    '[title*="Send"]',
    '[title*="send"]',
    'button[type="submit"]',
  ];

  for (const sel of selectors) {
    const btn = [...document.querySelectorAll(sel)].find(isUsable);
    if (btn) {
      humanClick(btn);
      return "clicked:" + sel;
    }
  }

  const inp = window.__copipeFindInput
    ? window.__copipeFindInput()
    : document.querySelector(
        '#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]',
      );
  if (inp) {
    const inpRect = inp.getBoundingClientRect();
    let el = inp.parentElement;
    for (let depth = 0; el && depth < 6; depth++, el = el.parentElement) {
      const btns = [...el.querySelectorAll("button, [role='button']")].filter(isUsable);
      for (const btn of btns) {
        const r = btn.getBoundingClientRect();
        const isNearRight = r.left >= inpRect.right - 20 && Math.abs(r.top - inpRect.top) < 160;
        const isNearBottom = r.top >= inpRect.bottom - 20 && Math.abs(r.left - inpRect.left) < 360;
        if (isNearRight || isNearBottom) {
          const clicked = humanClick(btn);
          return (
            "clicked:nearby@" +
            depth +
            ":" +
            (clicked.getAttribute("aria-label") ||
              clicked.getAttribute("data-testid") ||
              clicked.className.toString().slice(0, 30) ||
              "unknown")
          );
        }
      }
    }

    const form = inp.closest("form");
    if (form) {
      try {
        form.requestSubmit();
        return "form_requestSubmit";
      } catch (_) {}
    }
  }

  return null;
})();
