(function() {
    const el = window.__copipeFindInput ? window.__copipeFindInput() : document.querySelector('#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]');
    if (!el) return;

    const base = {
        key: 'Enter',
        code: 'Enter',
        keyCode: 13,
        which: 13,
        charCode: 0,
        bubbles: true,
        cancelable: true,
        composed: true,
        shiftKey: false,
        altKey: false,
        ctrlKey: false,
        metaKey: false
    };

    el.dispatchEvent(new KeyboardEvent('keydown',  base));
    el.dispatchEvent(new KeyboardEvent('keypress', { ...base, charCode: 13 }));
    el.dispatchEvent(new KeyboardEvent('keyup',    base));
})()
