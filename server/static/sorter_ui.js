/**
 * sorter2 web UI: fetch/eval for POST /ui. No product logic here.
 */
(function () {
  function evalJs(js) {
    if (js && String(js).trim()) {
      eval(js);
    }
  }

  function postUiForm(form) {
    return fetch(form.action, {
      method: 'POST',
      body: new URLSearchParams(new FormData(form)),
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      credentials: 'same-origin',
    }).then(function (resp) {
      return resp.text();
    }).then(evalJs);
  }

  function initSorterUi() {
    document.addEventListener('submit', async function (e) {
      var f = e.target;
      if (!f || f.tagName !== 'FORM') return;
      if ((f.method || 'get').toLowerCase() !== 'post') return;
      if (f.getAttribute('data-navigate') === 'full') return;
      e.preventDefault();
      await postUiForm(f);
      if (f.id === 'vote-form') {
        f.reset();
        var firstField = f.querySelector('input[type="text"]');
        if (firstField) firstField.focus();
      }
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initSorterUi);
  } else {
    initSorterUi();
  }
})();
