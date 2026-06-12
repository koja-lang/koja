// Fuzzy search for the doc sidebar. Loaded by every page; reads
// the per-page `data-root-prefix` attribute on `#doc-search` to
// route results back to the right relative root, then fetches
// `<root>/search-index.json` once and matches client-side.
//
// Match model: subsequence scoring against the joined `Pkg.Name`
// string for items and `Pkg.Type.method` for method entries. Ties
// break by shorter haystack first (prefer the exact type over a
// method match when the query equals the type name).
//
// Keyboard:
//   /        focus search (unless already typing in an input)
//   Escape   blur + clear results
//   ArrowUp/ArrowDown / Enter   navigate + open
(function () {
  var input = document.getElementById("doc-search");
  if (!input) return;
  var rootPrefix = input.getAttribute("data-root-prefix") || "";
  var resultsEl = document.getElementById("doc-search-results");
  if (!resultsEl) return;

  var index = null;
  var indexPromise = null;
  var selected = -1;
  var currentHits = [];

  function loadIndex() {
    if (index) return Promise.resolve(index);
    if (indexPromise) return indexPromise;
    indexPromise = fetch(rootPrefix + "search-index.json")
      .then(function (r) {
        return r.ok ? r.json() : [];
      })
      .then(function (data) {
        index = data || [];
        return index;
      })
      .catch(function () {
        index = [];
        return index;
      });
    return indexPromise;
  }

  // Subsequence scoring: returns null if `query` is not a
  // subsequence of `haystack` (case-insensitive). Otherwise returns
  // a score where lower is better. Bonuses for consecutive matches
  // and matches right after a separator (`.` or start-of-string).
  function score(query, haystack) {
    if (!query) return 0;
    var q = query.toLowerCase();
    var h = haystack.toLowerCase();
    var qi = 0;
    var hi = 0;
    var prevMatch = -2;
    var bonus = 0;
    while (qi < q.length && hi < h.length) {
      if (q.charCodeAt(qi) === h.charCodeAt(hi)) {
        if (hi === prevMatch + 1) bonus -= 3;
        if (hi === 0 || h.charAt(hi - 1) === ".") bonus -= 4;
        prevMatch = hi;
        qi++;
      }
      hi++;
    }
    if (qi < q.length) return null;
    return bonus + (haystack.length - q.length);
  }

  function render() {
    if (currentHits.length === 0) {
      resultsEl.innerHTML = "";
      resultsEl.classList.remove("open");
      return;
    }
    var html = "";
    for (var i = 0; i < currentHits.length; i++) {
      var hit = currentHits[i];
      var cls = "search-result" + (i === selected ? " selected" : "");
      html +=
        '<a class="' +
        cls +
        '" href="' +
        rootPrefix +
        escapeHtml(hit.url) +
        '">' +
        '<span class="type-chip chip-' +
        escapeHtml(hit.kind) +
        '">' +
        escapeHtml(hit.kind) +
        "</span>" +
        '<span class="search-result-name">' +
        escapeHtml(hit.pkg) +
        "." +
        escapeHtml(hit.name) +
        "</span>" +
        "</a>";
    }
    resultsEl.innerHTML = html;
    resultsEl.classList.add("open");
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function search(query) {
    selected = -1;
    if (!query.trim()) {
      currentHits = [];
      render();
      return;
    }
    loadIndex().then(function (items) {
      var scored = [];
      for (var i = 0; i < items.length; i++) {
        var it = items[i];
        var hay = it.pkg + "." + it.name;
        var s = score(query, hay);
        if (s !== null) scored.push({ hit: it, score: s });
      }
      scored.sort(function (a, b) {
        return a.score - b.score;
      });
      currentHits = scored.slice(0, 20).map(function (s) {
        return s.hit;
      });
      render();
    });
  }

  input.addEventListener("input", function () {
    search(input.value);
  });

  input.addEventListener("keydown", function (e) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (currentHits.length === 0) return;
      selected = (selected + 1) % currentHits.length;
      render();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (currentHits.length === 0) return;
      selected = (selected - 1 + currentHits.length) % currentHits.length;
      render();
    } else if (e.key === "Enter") {
      if (selected >= 0 && selected < currentHits.length) {
        e.preventDefault();
        window.location.href = rootPrefix + currentHits[selected].url;
      }
    } else if (e.key === "Escape") {
      input.value = "";
      input.blur();
      currentHits = [];
      selected = -1;
      render();
    }
  });

  document.addEventListener("keydown", function (e) {
    if (e.key !== "/" || e.ctrlKey || e.metaKey || e.altKey) return;
    var tag = document.activeElement && document.activeElement.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
    e.preventDefault();
    input.focus();
    input.select();
  });

  document.addEventListener("click", function (e) {
    if (!resultsEl.contains(e.target) && e.target !== input) {
      currentHits = [];
      selected = -1;
      render();
    }
  });
})();
