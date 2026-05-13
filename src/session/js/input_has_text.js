(function () {
  const inp = window.__copipeFindInput
    ? window.__copipeFindInput()
    : document.querySelector(
        '#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]',
      );
  if (!inp) return false;
  if ("value" in inp) return (inp.value || "").trim().length > 0;
  return (inp.innerText || inp.textContent || "").trim().length > 0;
})();
