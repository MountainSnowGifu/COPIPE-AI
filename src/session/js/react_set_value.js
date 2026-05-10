(function(promptText) {
    const el = document.querySelector('#userInput');
    if (!el) return 'not found';

    const nativeSetter = Object.getOwnPropertyDescriptor(
        window.HTMLTextAreaElement.prototype,
        'value'
    ).set;

    nativeSetter.call(el, promptText);

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