/* afpsql's live-monitor behaviour.
 *
 * AFUI owns the session protocol: state replacement, reconnect, call identity,
 * progress and terminal replies. This file knows only what an afpsql state or
 * EXPLAIN reply means to the page.
 */
(function () {
  'use strict';

  var snapshot = document.querySelector('[data-afpsql-monitor-snapshot]');
  var form = document.querySelector('[data-afpsql-explain-form]');
  var sql = document.querySelector('[data-afpsql-explain-sql]');
  var result = document.querySelector('[data-afpsql-explain-result]');
  var liveStatus = document.querySelector('[data-afpsql-live-status]');
  var explainStatus = document.querySelector('[data-afpsql-explain-status]');
  if (!snapshot || !form || !sql || !result || !liveStatus || !explainStatus) return;

  var live = afui.status(liveStatus);
  /* The connection word is AFUI's, so its appearance comes from the baseline;
   * this line only supplies the wording, and goes on carrying afpsql's own
   * updates through `live` above. */
  var showConnection = afui.connection(liveStatus, {
    connecting: 'Connecting…',
    live: 'Live',
    reconnecting: 'Reconnecting',
    closed: 'Disconnected',
  });
  var explainState = afui.status(explainStatus);
  var explaining = false;
  var observationTimer = null;

  /* This page's head, and the one it may be inside.
   *
   * A framed page carries two of them: the frame's, which already names the
   * program and the database this session is on, and this page's own saying it
   * again underneath. Told there is a head out there, this page drops its own
   * and sends up only the line the frame cannot know. */
  var pageHead = afui.frame({
    onHead: function (head) {
      document.body.dataset.frameHead = head ? 'true' : 'false';
    },
  });
  var submit = form.querySelector('button[type="submit"]');
  var submitLabel = submit ? submit.textContent : 'Explain';

  var runtime = afui.connect({
    onConnectionState: function (state) {
      /* Whatever the last update said — the time it arrived, or why it did
       * not — described a connection this page no longer has. */
      live.clear();
      showConnection(state);
      pageHead.status(liveStatus.textContent, state);
      if (submit) submit.disabled = explaining || state !== 'live';
      syncObservation();
    },
    onState: applyState,
  });

  /* PostgreSQL polling is demand-driven. A published session with no page
   * open keeps its already-rendered snapshot but does not keep querying the
   * server. A visible page renews a short controller-side lease; losing the
   * socket or being backgrounded simply lets that lease lapse. */
  function syncObservation() {
    if (observationTimer) clearInterval(observationTimer);
    observationTimer = null;
    if (!runtime || runtime.state !== 'live' || document.hidden) return;
    observe();
    observationTimer = setInterval(observe, 10000);
  }

  function observe() {
    if (runtime.state !== 'live' || document.hidden) return;
    runtime.call({type: 'observe'}).catch(function () {
      // Connection state is the single visible failure voice. A missed lease
      // only pauses database refreshes until the next successful check-in.
    });
  }

  document.addEventListener('visibilitychange', syncObservation);
  window.addEventListener('pagehide', function () {
    if (observationTimer) clearInterval(observationTimer);
    observationTimer = null;
  });

  function setLive(text, state) {
    live.set(text, state);
    pageHead.status(text, state);
  }

  function setExplainPending(pending, text) {
    explaining = pending;
    form.setAttribute('aria-busy', pending ? 'true' : 'false');
    explainState.set(text || '');
    if (submit) {
      submit.disabled = pending || runtime.state !== 'live';
      submit.textContent = pending ? 'Planning…' : submitLabel;
    }
  }

  function showExplainError(message) {
    result.hidden = false;
    result.dataset.state = 'error';
    result.textContent = message;
    setExplainPending(false, 'Couldn’t build the plan');
  }

  function applyState(state) {
    if (!state) return;
    if (state.type === 'snapshot' && typeof state.page_html === 'string') {
      replaceSnapshot(state.page_html);
      setLive('Live · updated ' + new Date().toLocaleTimeString());
      return;
    }
    if (state.type === 'error') {
      setLive(state.message || 'Live update failed', 'error');
    }
  }

  function applyReply(reply) {
    if (!reply) return;
    if (reply.type === 'explain_started') {
      result.hidden = false;
      delete result.dataset.state;
      result.textContent = 'Planning…';
      return;
    }
    if (reply.type === 'explain_result') {
      result.hidden = false;
      delete result.dataset.state;
      result.textContent = Array.isArray(reply.plan_lines)
        ? reply.plan_lines.join('\n')
        : 'PostgreSQL returned an empty plan.';
      setExplainPending(false, 'Plan ready');
    }
  }

  form.addEventListener('submit', function (event) {
    event.preventDefault();
    if (runtime.state !== 'live' || explaining || !form.reportValidity()) return;
    result.hidden = false;
    delete result.dataset.state;
    result.textContent = 'Planning…';
    setExplainPending(true, 'Asking PostgreSQL…');
    runtime.call(
      {type: 'explain', sql: sql.value},
      {onProgress: applyReply},
    ).then(applyReply).catch(function (failure) {
      showExplainError(failure.message || 'The request could not be completed.');
    });
  });

  sql.addEventListener('keydown', function (event) {
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      form.requestSubmit();
    }
  });

  function replaceSnapshot(pageHtml) {
    var parsed = new DOMParser().parseFromString(pageHtml, 'text/html');
    var next = parsed.querySelector('[data-afpsql-monitor-snapshot]');
    if (!next) {
      setLive('Incomplete live update', 'error');
      return;
    }
    var currentExplain = snapshot.querySelector('[data-afpsql-explain-panel]');
    var nextExplain = next.querySelector('[data-afpsql-explain-panel]');
    if (currentExplain && nextExplain) nextExplain.replaceWith(currentExplain);
    snapshot.replaceChildren.apply(snapshot, Array.from(next.childNodes));
  }
})();
