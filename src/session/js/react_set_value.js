(function (promptText) {
  const el = window.__copipeFindInput
    ? window.__copipeFindInput()
    : document.querySelector(
        '#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]',
      );
  if (!el) return "not found";

  el.focus();

  // ① クリップボードペースト経由（最も自然。React の SyntheticEvent も拾う）
  try {
    const dt = new DataTransfer();
    dt.setData("text/plain", promptText);
    const paste = new ClipboardEvent("paste", {
      bubbles: true,
      cancelable: true,
      composed: true,
      clipboardData: dt,
    });
    el.dispatchEvent(paste);
    const val =
      "value" in el ? el.value : el.innerText || el.textContent || "";
    if (val.length > 0) return "clipboard_paste_ok";
  } catch (_) {}

  // ② React native setter（SPA が value を管理している場合）
  const lastChar = promptText.slice(-1) || "x";
  if ("value" in el) {
    const proto =
      el instanceof HTMLTextAreaElement
        ? window.HTMLTextAreaElement.prototype
        : window.HTMLInputElement.prototype;
    const nativeSetter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
    if (nativeSetter) {
      nativeSetter.call(el, promptText);
    } else {
      el.value = promptText;
    }
  } else {
    el.textContent = promptText;
  }

  // beforeinput → input → change の順（ブラウザの自然な発火順序）
  try {
    el.dispatchEvent(
      new InputEvent("beforeinput", {
        bubbles: true,
        cancelable: true,
        composed: true,
        inputType: "insertFromPaste",
        data: promptText,
      }),
    );
  } catch (_) {}

  el.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
  el.dispatchEvent(new Event("change", { bubbles: true, composed: true }));

  try {
    el.dispatchEvent(
      new InputEvent("input", {
        bubbles: true,
        composed: true,
        inputType: "insertText",
        data: lastChar,
      }),
    );
  } catch (_) {}

  return "react_setter_ok";
})
