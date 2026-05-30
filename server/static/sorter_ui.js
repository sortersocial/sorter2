/**
 * sorter2 web UI: POST /ui returns JS (morph) or SSE (entity fetch).
 */
(function () {
  function evalJs(js) {
    if (js && String(js).trim()) {
      eval(js);
    }
  }

  function morphSelector(selector, html) {
    var el = document.querySelector(selector);
    if (el && typeof Idiomorph !== 'undefined') {
      Idiomorph.morph(el, html);
    }
  }

  function handleSseEvent(eventType, data, form) {
    if (eventType === 'fetching' || eventType === 'complete') {
      try {
        var msg = JSON.parse(data);
        morphSelector(msg.selector || '#entity-section', msg.html);
      } catch (err) {
        console.warn('fetch morph parse', err);
      }
    }
    if (eventType === 'complete' || eventType === 'error') {
      var btn = form && form.querySelector('button[type="submit"]');
      if (btn) btn.disabled = false;
    }
    if (eventType === 'error') {
      try {
        var err = JSON.parse(data);
        console.warn('fetch error:', err.message || data);
      } catch (_e) {
        console.warn('fetch error:', data);
      }
    }
  }

  function consumeSseStream(response, form) {
    var reader = response.body.getReader();
    var decoder = new TextDecoder();
    var buffer = '';
    var eventType = '';
    var dataLines = [];

    function dispatch() {
      if (!eventType && dataLines.length === 0) return;
      handleSseEvent(eventType || 'message', dataLines.join('\n'), form);
      eventType = '';
      dataLines = [];
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
          } else if (line.indexOf('event:') === 0) {
            eventType = line.slice(6).trim();
          } else if (line.indexOf('data:') === 0) {
            dataLines.push(line.slice(5).trim());
          }
        }
        return pump();
      });
    }

    return pump();
  }

  function postUiForm(form) {
    var btn = form.querySelector('button[type="submit"]');
    if (form.id === 'fetch-entity-form' && btn) {
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
        return consumeSseStream(resp, form);
      }
      return resp.text().then(evalJs);
    }).catch(function (err) {
      if (form.id === 'fetch-entity-form' && btn) {
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
