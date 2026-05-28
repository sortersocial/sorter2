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

  var parserTimer = null;

  function scheduleParserInput(input) {
    if (parserTimer) clearTimeout(parserTimer);
    parserTimer = setTimeout(function () {
      var form = document.getElementById('parser-form');
      if (form) postUiForm(form);
    }, 120);
  }

  function initSorterUi() {
    document.addEventListener('submit', async function (e) {
      var f = e.target;
      if (!f || f.tagName !== 'FORM') return;
      if ((f.method || 'get').toLowerCase() !== 'post') return;
      if (f.id === 'sorter-theme-form') return;
      if (f.getAttribute('data-navigate') === 'full') return;
      e.preventDefault();
      await postUiForm(f);
    });

    document.addEventListener('input', function (e) {
      if (e.target && e.target.id === 'parser-input') {
        scheduleParserInput(e.target);
      }
    });

    document.addEventListener('keydown', function (e) {
      if (!e.target || e.target.id !== 'parser-input') return;
      if (e.key !== 'Tab') return;
      var completion =
        e.target.dataset.completion ||
        (function () {
          var btn = document.querySelector('#parser-output .parser-suggestion-primary');
          return btn && btn.getAttribute('data-completion');
        })();
      if (!completion) return;
      e.preventDefault();
      e.target.value = completion;
      scheduleParserInput(e.target);
    });

    document.addEventListener('click', function (e) {
      var btn = e.target.closest('.parser-completion');
      if (!btn) return;
      var input = document.getElementById('parser-input');
      if (!input) return;
      var completion = btn.getAttribute('data-completion');
      if (!completion) return;
      input.value = completion;
      scheduleParserInput(input);
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initSorterUi);
  } else {
    initSorterUi();
  }
})();
