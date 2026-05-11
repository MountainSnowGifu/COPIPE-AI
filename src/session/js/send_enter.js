(function () {
  const el = window.__copipeFindInput
    ? window.__copipeFindInput()
    : document.querySelector(
        '#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]',
      );
  if (!el) return "no_input";

  el.focus();
  const base = {
    key: "Enter",
    code: "Enter",
    keyCode: 13,
    which: 13,
    charCode: 0,
    bubbles: true,
    cancelable: true,
    composed: true,
    shiftKey: false,
    altKey: false,
    ctrlKey: false,
    metaKey: false,
  };

  const targets = [el, document.activeElement, document].filter(Boolean);
  for (const target of targets) {
    target.dispatchEvent(new KeyboardEvent("keydown", base));
    target.dispatchEvent(new KeyboardEvent("keypress", { ...base, charCode: 13 }));
    target.dispatchEvent(new KeyboardEvent("keyup", base));
  }

  const form = el.closest("form");
  if (form) {
    try {
      form.requestSubmit();
      return "enter_and_requestSubmit";
    } catch (_) {}
  }

  return "enter_events";
})();
