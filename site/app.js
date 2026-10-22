/* types.cartercole.net — background rain, clock, terminal line */
(function () {
  "use strict";

  var reduced = window.matchMedia &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ---------- UTC clock ---------- */

  var clockEl = document.getElementById("clock");

  function pad(n) { return n < 10 ? "0" + n : "" + n; }

  function tick() {
    var d = new Date();
    clockEl.textContent =
      pad(d.getUTCHours()) + ":" + pad(d.getUTCMinutes()) + ":" +
      pad(d.getUTCSeconds()) + " UTC";
  }

  if (clockEl) { tick(); setInterval(tick, 1000); }

  var yearEl = document.getElementById("year");
  if (yearEl) { yearEl.textContent = String(new Date().getUTCFullYear()); }

  /* ---------- terminal line ---------- */

  var typedEl = document.getElementById("typed");
  var LINES = [
    "geo::Point and gfx::Vec2 are proof-mystery-trouble. nobody told it.",
    "List<i64> is provide-future-cave, in every language.",
    "one proposition, three proofs, chosen at the call.",
    "735 tests. one specification. static node, no telemetry."
  ];

  if (typedEl) {
    if (reduced) {
      typedEl.textContent = LINES[0];
    } else {
      var li = 0, ci = 0, erasing = false;

      (function step() {
        var line = LINES[li];

        if (!erasing) {
          typedEl.textContent = line.slice(0, ++ci);
          if (ci >= line.length) {
            erasing = true;
            return setTimeout(step, 3600);
          }
          return setTimeout(step, 34 + Math.random() * 42);
        }

        typedEl.textContent = line.slice(0, --ci);
        if (ci <= 0) {
          erasing = false;
          li = (li + 1) % LINES.length;
          return setTimeout(step, 420);
        }
        return setTimeout(step, 16);
      })();
    }
  }

  /* ---------- glyph rain ---------- */

  var canvas = document.getElementById("rain");
  if (!canvas || !canvas.getContext) { return; }

  var ctx = canvas.getContext("2d");
  var GLYPHS = "01101001アカサタナハマヤラワ" +
               "0123456789ABCDEF¥§<>/\\[]{}=+*".split("");
  var SIZE = 13;
  var cols = 0, rows = 0, drops = [], w = 0, h = 0;

  function resize() {
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    w = window.innerWidth;
    h = window.innerHeight;
    canvas.width = Math.floor(w * dpr);
    canvas.height = Math.floor(h * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.font = SIZE + "px 'JetBrains Mono', monospace";
    ctx.textBaseline = "top";

    cols = Math.ceil(w / SIZE);
    rows = Math.ceil(h / SIZE);

    drops = new Array(cols);
    for (var i = 0; i < cols; i++) {
      drops[i] = Math.floor(Math.random() * -rows);
    }

    ctx.fillStyle = "#04060a";
    ctx.fillRect(0, 0, w, h);
  }

  function glyph() { return GLYPHS[(Math.random() * GLYPHS.length) | 0]; }

  function frame() {
    /* trail: fade what is already drawn */
    ctx.fillStyle = "rgba(4, 6, 10, 0.085)";
    ctx.fillRect(0, 0, w, h);

    for (var i = 0; i < cols; i++) {
      var y = drops[i];
      if (y >= 0 && y <= rows) {
        var x = i * SIZE;

        /* bright head, occasionally amber */
        ctx.fillStyle = Math.random() < 0.06
          ? "rgba(255, 179, 71, 0.85)"
          : "rgba(140, 250, 244, 0.8)";
        ctx.fillText(glyph(), x, y * SIZE);

        /* dim body one cell up */
        if (y > 0) {
          ctx.fillStyle = "rgba(53, 231, 224, 0.26)";
          ctx.fillText(glyph(), x, (y - 1) * SIZE);
        }
      }

      drops[i] = y > rows && Math.random() > 0.975
        ? Math.floor(Math.random() * -24)
        : y + 1;
    }
  }

  resize();

  var resizeTimer;
  window.addEventListener("resize", function () {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(resize, 180);
  });

  if (reduced) {
    /* one static pass, no animation */
    for (var p = 0; p < 26; p++) { frame(); }
    return;
  }

  var last = 0;
  (function loop(now) {
    if (now - last > 55) { frame(); last = now; }
    requestAnimationFrame(loop);
  })(0);
})();
