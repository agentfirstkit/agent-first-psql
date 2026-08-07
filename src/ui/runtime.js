/* afpsql's decision runtime: the half of a decide panel a frontend cannot write.
 *
 * A template declares what a control is for — `data-afpsql-decision="approve"`.
 * This is what makes that declaration do anything, and it is served inline
 * under a per-session nonce that no frontend file can forge. So an override may
 * move the controls, relabel them, wrap them, drop everything else on the page,
 * and still cannot make a control submit an answer other than the one it
 * declared: the mapping from declaration to route lives here, in afpsql.
 *
 * A declaration afpsql does not recognise binds to nothing. Nothing on this page
 * approves by default, and nothing approves by omission — with scripting off,
 * no control submits at all, and a window that submits nothing is refused.
 */
(function () {
  var routes = { approve: "approve", refuse: "refuse" };
  var controls = document.querySelectorAll("[data-afpsql-decision]");
  var index = 0;
  for (index = 0; index < controls.length; index += 1) {
    bind(controls[index]);
  }

  function bind(control) {
    var declared = control.getAttribute("data-afpsql-decision");
    if (!Object.prototype.hasOwnProperty.call(routes, declared)) {
      return;
    }
    var action = routes[declared];
    control.addEventListener("click", function (event) {
      event.preventDefault();
      /* A form rather than fetch: the answer is a navigation, so the page a
       * person ends on is afpsql's own reply, and the policy that governs it is
       * `form-action 'self'` rather than a connect-src exemption. */
      var form = document.createElement("form");
      form.method = "post";
      form.action = action;
      document.body.appendChild(form);
      form.submit();
    });
  }
})();
