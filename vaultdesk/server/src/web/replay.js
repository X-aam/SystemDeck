// Runs inside the /replay iframe. It holds no data of its own: the parent
// dashboard (already authenticated) fetches the recording and posts the rrweb
// events in. We rebuild and play them with rrweb-player.
(function () {
  "use strict";
  var holder = document.getElementById("p");

  function play(events, width, height) {
    holder.textContent = "";
    try {
      // eslint-disable-next-line no-new
      new rrwebPlayer({
        target: holder,
        props: {
          events: events,
          width: width || 900,
          height: height || 600,
          autoPlay: false,
          showController: true,
        },
      });
    } catch (err) {
      holder.textContent = "Cannot replay this recording: " + ((err && err.message) || err);
    }
  }

  window.addEventListener("message", function (e) {
    if (e.origin !== location.origin) return;
    var m = e.data || {};
    if (m.type === "events" && Array.isArray(m.events)) play(m.events, m.width, m.height);
  });

  // Signal the parent that we are ready to receive the events.
  if (window.parent) window.parent.postMessage({ type: "replay-ready" }, location.origin);
})();
