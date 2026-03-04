/* Moduvex Landing — Theme, i18n, Scroll, Copy */

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

    document.querySelectorAll("[data-i18n]").forEach(function (el) {
      var key = el.getAttribute("data-i18n");
      if (dict[key] !== undefined) el.textContent = dict[key];
    });

    document.querySelectorAll("[data-i18n-html]").forEach(function (el) {
      var key = el.getAttribute("data-i18n-html");
      if (dict[key] !== undefined) el.innerHTML = dict[key];
    });

    document.documentElement.setAttribute("lang", lang);
    if (langSelect) langSelect.value = lang;
    localStorage.setItem(LANG_KEY, lang);
  }

  function detectLang() {
    var saved = localStorage.getItem(LANG_KEY);
    if (saved && SUPPORTED.indexOf(saved) !== -1) return saved;
    var nav = (navigator.language || "").toLowerCase();
    for (var i = 0; i < SUPPORTED.length; i++) {
      if (nav.indexOf(SUPPORTED[i]) === 0) return SUPPORTED[i];
    }
    return "en";
  }

  function initLang() { applyLang(detectLang()); }

  if (langSelect) {
    langSelect.addEventListener("change", function () { applyLang(this.value); });
  }

  initLang();

  /* ── Smooth Scroll ── */
  document.querySelectorAll('a[href^="#"]').forEach(function (link) {
    link.addEventListener("click", function (e) {
      var id = this.getAttribute("href").slice(1);
      var target = document.getElementById(id);
      if (target) {
        e.preventDefault();
        target.scrollIntoView({ behavior: "smooth", block: "start" });
        // Close mobile nav if open
        var navLinks = document.getElementById("nav-links");
        if (navLinks) navLinks.classList.remove("open");
      }
    });
  });

  /* ── Fade-in on Scroll ── */
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
      { threshold: 0.1 }
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

  /* ── Copy Code Buttons ── */
  var codeSnippets = {
    cargo: '[dependencies]\nmoduvex-starter-web = "0.1"',
    main: 'use moduvex_starter_web::prelude::*;\n\n#[moduvex::main]\nasync fn main() {\n    info!("Starting server");\n    Moduvex::new()\n        .module::<HelloModule>()\n        .run()\n        .await;\n}'
  };

  document.querySelectorAll(".code-copy").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var key = this.getAttribute("data-code");
      var text = codeSnippets[key] || "";
      if (!text) return;

      var button = this;
      navigator.clipboard.writeText(text).then(function () {
        button.classList.add("copied");
        button.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M20 6L9 17l-5-5"/></svg>';
        setTimeout(function () {
          button.classList.remove("copied");
          button.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
        }, 2000);
      });
    });
  });

  /* ── Nav shrink on scroll ── */
  var nav = document.querySelector(".nav");
  if (nav) {
    window.addEventListener("scroll", function () {
      if (window.scrollY > 50) {
        nav.style.boxShadow = "0 1px 8px rgba(0,0,0,0.08)";
      } else {
        nav.style.boxShadow = "none";
      }
    }, { passive: true });
  }
})();
