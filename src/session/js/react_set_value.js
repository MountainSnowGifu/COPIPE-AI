(function(promptText) {
    const el = window.__copipeFindInput ? window.__copipeFindInput() : document.querySelector('#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]');
    if (!el) return 'not found';

    if ('value' in el) {
        const proto = el instanceof HTMLTextAreaElement
            ? window.HTMLTextAreaElement.prototype
            : window.HTMLInputElement.prototype;
        const nativeSetter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
        if (nativeSetter) {
            nativeSetter.call(el, promptText);
        } else {
            el.value = promptText;
        }
    } else {
        el.focus();
        el.textContent = promptText;
    }

    el.dispatchEvent(new Event('input',  { bubbles: true, composed: true }));
    el.dispatchEvent(new Event('change', { bubbles: true, composed: true }));

    try {
        el.dispatchEvent(new InputEvent('input', {
            bubbles: true,
            composed: true,
            inputType: 'insertText',
            data: 'x'
        }));
    } catch (_) {}

    return 'react_setter_ok';
})
