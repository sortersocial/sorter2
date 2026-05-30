/**
 * sorter2 web UI: POST /ui returns JS (morph) or SSE (entity fetch).
 */
(function () {
  function evalJs(js) {
    if (js && String(js).trim()) {
      eval(js);
    }
  }

  // Each SSE event's `data` is a JS snippet to eval (same as the non-stream
  // /ui responses). Parse the raw event stream, joining multi-line `data:`
  // fields, and eval each event as it arrives.
  function consumeSseStream(response) {
    var reader = response.body.getReader();
    var decoder = new TextDecoder();
    var buffer = '';
    var dataLines = [];

    function dispatch() {
      if (dataLines.length === 0) return;
      var js = dataLines.join('\n');
      dataLines = [];
      try {
        evalJs(js);
      } catch (err) {
        console.warn('fetch eval failed', err);
      }
    }

    function pump() {
      return reader.read().then(function (chunk) {
        if (chunk.done) {
          dispatch();
          return;
        }
        buffer += decoder.decode(chunk.value, { stream: true });
        var parts = buffer.split('\n');
        buffer = parts.pop() || '';
        for (var i = 0; i < parts.length; i++) {
          var line = parts[i].replace(/\r$/, '');
          if (line === '') {
            dispatch();
          } else if (line.indexOf('data:') === 0) {
            dataLines.push(line.slice(5).replace(/^ /, ''));
          }
          // `event:`/`id:`/`:` comment lines are ignored — data carries the JS.
        }
        return pump();
      });
    }

    return pump();
  }

  function isFetchForm(form) {
    return form.classList && form.classList.contains('fetch-entity-form');
  }

  function postUiForm(form) {
    var btn = form.querySelector('button[type="submit"]');
    if (isFetchForm(form) && btn) {
      btn.disabled = true;
    }
    return fetch(form.action, {
      method: 'POST',
      body: new URLSearchParams(new FormData(form)),
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      credentials: 'same-origin',
    }).then(function (resp) {
      var ct = resp.headers.get('content-type') || '';
      if (ct.indexOf('text/event-stream') !== -1) {
        return consumeSseStream(resp);
      }
      return resp.text().then(evalJs);
    }).catch(function (err) {
      if (isFetchForm(form) && btn) {
        btn.disabled = false;
      }
      console.warn('POST /ui failed', err);
    });
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
