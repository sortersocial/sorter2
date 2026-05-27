/**
 * Slug web UI: only plumbing — fetch/eval/SSE. No product UI logic here.
 */
(function () {
  function evalJs(js) {
    if (js && String(js).trim()) {
      eval(js);
    }
  }

  // Theme cookie sync (runs before paint; full reload if localStorage disagrees with cookie)

  function initSlugUi() {
    // POST forms → eval response (except theme + full-navigation forms)
    document.addEventListener('submit', async function (e) {
      var f = e.target;
      if (!f || f.tagName !== 'FORM') return;
      if ((f.method || 'get').toLowerCase() !== 'post') return;
      if (f.id === 'slug-theme-form') return;
      if (f.getAttribute('data-navigate') === 'full') return;
      e.preventDefault();
      var resp = await fetch(f.action, {
        method: 'POST',
        body: new URLSearchParams(new FormData(f)),
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        credentials: 'same-origin',
      });
      evalJs(await resp.text());
    });

    // SSE: server-pushed JS
    function connectSSE() {
      var ssePath = window.location.pathname + window.location.search;
      var es = new EventSource('/sse?path=' + encodeURIComponent(ssePath));
      es.onmessage = function (e) {
        evalJs(e.data);
      };
      es.onerror = function () {
        es.close();
        setTimeout(connectSSE, 3000);
      };
    }
    connectSSE();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initSlugUi);
  } else {
    initSlugUi();
  }
})();

