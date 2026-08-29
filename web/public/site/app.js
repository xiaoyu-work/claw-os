// Claw OS website — minimal, framework-free interactions.

(function () {
  "use strict";

  // ---------- Sticky nav shadow ----------
  const nav = document.getElementById("nav");
  if (nav) {
    const onScroll = () => nav.classList.toggle("is-scrolled", window.scrollY > 8);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
  }

  // ---------- Mobile nav ----------
  const burger = document.getElementById("navBurger");
  if (burger && nav) {
    burger.addEventListener("click", () => {
      const open = nav.classList.toggle("is-mobile-open");
      burger.setAttribute("aria-expanded", open ? "true" : "false");
    });
    nav.querySelectorAll(".nav-links a").forEach((link) =>
      link.addEventListener("click", () => {
        nav.classList.remove("is-mobile-open");
        burger.setAttribute("aria-expanded", "false");
      })
    );
  }

  // ---------- Copy-to-clipboard ----------
  const flashCopied = (button) => {
    const previousText = button.textContent;
    button.textContent = "copied";
    button.classList.add("is-copied");
    setTimeout(() => {
      button.textContent = previousText;
      button.classList.remove("is-copied");
    }, 1400);
  };

  document.querySelectorAll(".copy-btn").forEach((button) => {
    button.addEventListener("click", async () => {
      const selector = button.getAttribute("data-copy");
      const target = selector ? document.querySelector(selector) : null;
      if (!target) return;
      const text = target.innerText.replace(/\u00A0/g, " ").trim();
      try {
        await navigator.clipboard.writeText(text);
        flashCopied(button);
      } catch (_) {
        const range = document.createRange();
        range.selectNodeContents(target);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        try {
          document.execCommand("copy");
          flashCopied(button);
        } catch (_) {
          // The browser does not expose a clipboard fallback.
        }
        selection.removeAllRanges();
      }
    });
  });
})();
