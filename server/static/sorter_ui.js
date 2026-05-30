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

  function rankRowPositions(root) {
    var positions = {};
    if (!root) return positions;
    root.querySelectorAll('[data-rank-item]').forEach(function (row) {
      positions[row.getAttribute('data-rank-item')] = row.getBoundingClientRect();
    });
    return positions;
  }

  window.sorter2MorphWithFlip = function (selector, html) {
    var root = document.querySelector(selector);
    if (!root) return;
    var before = rankRowPositions(root);
    Idiomorph.morph(root, html);
    var afterRoot = document.querySelector(selector);
    if (!afterRoot) return;
    afterRoot.querySelectorAll('[data-rank-item]').forEach(function (row) {
      var key = row.getAttribute('data-rank-item');
      var oldBox = before[key];
      if (!oldBox) {
        row.classList.add('rank-row-enter');
        requestAnimationFrame(function () {
          row.classList.remove('rank-row-enter');
        });
        return;
      }
      var newBox = row.getBoundingClientRect();
      var dy = oldBox.top - newBox.top;
      if (Math.abs(dy) < 1) return;
      row.style.transform = 'translateY(' + dy + 'px)';
      row.style.transition = 'transform 0s';
      requestAnimationFrame(function () {
        row.style.transition = 'transform 260ms ease';
        row.style.transform = '';
      });
    });
  };

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

  function initVoteSlider() {
    var slider = document.getElementById('vote-preference-slider');
    if (!slider) return;
    var leftInput = document.getElementById('vote-ratio-left');
    var rightInput = document.getElementById('vote-ratio-right');
    var ratioDisplay = document.getElementById('vote-ratio-display');
    function gcd(a, b) {
      a = Math.abs(a);
      b = Math.abs(b);
      while (b) {
        var t = b;
        b = a % b;
        a = t;
      }
      return a || 1;
    }
    function update() {
      var v = parseInt(slider.value, 10);
      if (!Number.isFinite(v)) v = 50;
      var left = Math.max(1, 100 - v);
      var right = Math.max(1, v);
      var divisor = gcd(left, right);
      left = left / divisor;
      right = right / divisor;
      if (leftInput) leftInput.value = String(left);
      if (rightInput) rightInput.value = String(right);
      if (ratioDisplay) ratioDisplay.textContent = left + ':' + right;
    }
    slider.addEventListener('input', update);
    update();
  }

  function initSorterUi() {
    initVoteSlider();
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
