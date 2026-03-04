/* Moduvex Landing — Theme Toggle + Scroll Animations + i18n */

(function () {
  "use strict";

  /* ── Theme Toggle ── */
  var THEME_KEY = "moduvex-theme";
  var toggle = document.getElementById("theme-toggle");

  function applyTheme(theme) {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem(THEME_KEY, theme);
    if (toggle) toggle.setAttribute("aria-label", "Switch to " + (theme === "dark" ? "light" : "dark") + " mode");
  }

  function initTheme() {
    var saved = localStorage.getItem(THEME_KEY);
    if (saved) return applyTheme(saved);
    var prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    applyTheme(prefersDark ? "dark" : "light");
  }

  if (toggle) {
    toggle.addEventListener("click", function () {
      var current = document.documentElement.getAttribute("data-theme");
      applyTheme(current === "dark" ? "light" : "dark");
    });
  }

  initTheme();

  /* ── i18n ── */
  var LANG_KEY = "moduvex-lang";
  var SUPPORTED = ["en", "vi", "zh", "ja", "ko"];
  var langSelect = document.getElementById("lang-select");

  function applyLang(lang) {
    if (!TRANSLATIONS || !TRANSLATIONS[lang]) return;
    var dict = TRANSLATIONS[lang];

    /* textContent elements */
    document.querySelectorAll("[data-i18n]").forEach(function (el) {
      var key = el.getAttribute("data-i18n");
      if (dict[key] !== undefined) el.textContent = dict[key];
    });

    /* innerHTML elements (contain markup like <code>, <span>) */
    document.querySelectorAll("[data-i18n-html]").forEach(function (el) {
      var key = el.getAttribute("data-i18n-html");
      if (dict[key] !== undefined) el.innerHTML = dict[key];
    });

    document.documentElement.setAttribute("lang", lang);
    if (langSelect) langSelect.value = lang;
    localStorage.setItem(LANG_KEY, lang);
  }

  function detectLang() {
    /* 1. localStorage */
    var saved = localStorage.getItem(LANG_KEY);
    if (saved && SUPPORTED.indexOf(saved) !== -1) return saved;

    /* 2. navigator.language */
    var nav = (navigator.language || "").toLowerCase();
    for (var i = 0; i < SUPPORTED.length; i++) {
      if (nav.indexOf(SUPPORTED[i]) === 0) return SUPPORTED[i];
    }

    /* 3. fallback */
    return "en";
  }

  function initLang() {
    var lang = detectLang();
    applyLang(lang);
  }

  if (langSelect) {
    langSelect.addEventListener("change", function () {
      applyLang(this.value);
    });
  }

  initLang();

  /* ── Smooth Scroll for Anchor Links ── */
  document.querySelectorAll('a[href^="#"]').forEach(function (link) {
    link.addEventListener("click", function (e) {
      var id = this.getAttribute("href").slice(1);
      var target = document.getElementById(id);
      if (target) {
        e.preventDefault();
        target.scrollIntoView({ behavior: "smooth", block: "start" });
      }
    });
  });

  /* ── Fade-in on Scroll (IntersectionObserver) ── */
  var animatedEls = document.querySelectorAll(".fade-in");

  if ("IntersectionObserver" in window && animatedEls.length) {
    var observer = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting) {
            entry.target.classList.add("visible");
            observer.unobserve(entry.target);
          }
        });
      },
      { threshold: 0.15 }
    );
    animatedEls.forEach(function (el) { observer.observe(el); });
  }

  /* ── Mobile Nav Toggle ── */
  var navToggle = document.getElementById("nav-toggle");
  var navLinks = document.getElementById("nav-links");
  if (navToggle && navLinks) {
    navToggle.addEventListener("click", function () {
      navLinks.classList.toggle("open");
      navToggle.setAttribute("aria-expanded", navLinks.classList.contains("open"));
    });
  }
})();
