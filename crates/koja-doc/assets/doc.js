// Doc-page chrome, loaded deferred on every page. Wires up the
// header theme toggle, the mobile rail toggle, and the right-TOC
// scroll spy.
(function () {
  var toggle = document.querySelector(".theme-toggle");
  if (toggle) {
    toggle.addEventListener("click", function () {
      var next =
        document.documentElement.dataset.theme === "dark" ? "light" : "dark";
      document.documentElement.dataset.theme = next;
      localStorage.setItem("theme", next);
    });
  }

  // The sticky header draws its bottom border only once content has
  // scrolled underneath it. A 1px sentinel above the header leaves the
  // viewport as soon as the page scrolls.
  var topbar = document.querySelector(".topbar");
  if (topbar && "IntersectionObserver" in window) {
    var sentinel = document.createElement("div");
    sentinel.style.cssText =
      "position: absolute; top: 0; left: 0; width: 1px; height: 1px;";
    document.body.prepend(sentinel);
    new IntersectionObserver(function (entries) {
      topbar.classList.toggle("scrolled", !entries[0].isIntersecting);
    }).observe(sentinel);
  }

  var railToggle = document.querySelector(".rail-toggle");
  var rail = document.getElementById("rail");
  if (railToggle && rail) {
    railToggle.addEventListener("click", function () {
      var open = rail.classList.toggle("open");
      railToggle.setAttribute("aria-expanded", open ? "true" : "false");
    });
  }

  // Scroll spy. Marks the last TOC target above the fold line.
  var tocLinks = Array.prototype.slice.call(
    document.querySelectorAll(".toc a[href^='#']"),
  );
  if (tocLinks.length === 0) return;
  var targets = [];
  tocLinks.forEach(function (link) {
    var el = document.getElementById(link.getAttribute("href").slice(1));
    if (el) targets.push({ el: el, link: link });
  });

  function spy() {
    var current = null;
    for (var i = 0; i < targets.length; i++) {
      if (targets[i].el.getBoundingClientRect().top <= 96) {
        current = targets[i].link;
      }
    }
    tocLinks.forEach(function (link) {
      link.removeAttribute("aria-current");
    });
    if (current) current.setAttribute("aria-current", "true");
  }

  document.addEventListener("scroll", spy, { passive: true });
  spy();
})();
