// Claw OS site — minimal, framework-free interactions.

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
    nav.querySelectorAll(".nav-links a").forEach((a) =>
      a.addEventListener("click", () => {
        nav.classList.remove("is-mobile-open");
        burger.setAttribute("aria-expanded", "false");
      })
    );
  }

  // ---------- Copy-to-clipboard ----------
  const flashCopied = (btn) => {
    const prev = btn.textContent;
    btn.textContent = "copied";
    btn.classList.add("is-copied");
    setTimeout(() => {
      btn.textContent = prev;
      btn.classList.remove("is-copied");
    }, 1400);
  };
  document.querySelectorAll(".copy-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const sel = btn.getAttribute("data-copy");
      const target = sel ? document.querySelector(sel) : null;
      if (!target) return;
      const text = target.innerText.replace(/\u00A0/g, " ").trim();
      try {
        await navigator.clipboard.writeText(text);
        flashCopied(btn);
      } catch (_) {
        const r = document.createRange();
        r.selectNodeContents(target);
        const sel2 = window.getSelection();
        sel2.removeAllRanges();
        sel2.addRange(r);
        try { document.execCommand("copy"); flashCopied(btn); } catch (e) { /* noop */ }
        sel2.removeAllRanges();
      }
    });
  });

  // ---------- Demo tabs ----------
  const tabs = document.querySelectorAll(".demo-tab");
  const panes = document.querySelectorAll(".demo-pane");
  const address = document.getElementById("demoAddress");
  const labels = {
    service: "cos · agent ask · service repair",
    browser: "cos · agent ask · browser triage",
    schedule:"cos · agent ask · scheduled task",
    model:   "cos · model · local inference",
  };
  const activate = (name) => {
    tabs.forEach((t) => {
      const active = t.dataset.tab === name;
      t.classList.toggle("is-active", active);
      t.setAttribute("aria-selected", active ? "true" : "false");
    });
    panes.forEach((p) => p.classList.toggle("is-active", p.dataset.pane === name));
    if (address && labels[name]) address.textContent = labels[name];
  };
  tabs.forEach((t) => t.addEventListener("click", () => activate(t.dataset.tab)));

  // Auto-rotate when the demo is on screen and the user isn't hovering.
  const demo = document.querySelector(".demo-shell");
  if (demo && tabs.length > 1 && !window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    let hovered = false;
    let onScreen = false;
    demo.addEventListener("mouseenter", () => (hovered = true));
    demo.addEventListener("mouseleave", () => (hovered = false));
    const io = new IntersectionObserver(
      (entries) => entries.forEach((e) => (onScreen = e.isIntersecting)),
      { threshold: 0.5 }
    );
    io.observe(demo);
    let userPicked = false;
    tabs.forEach((t) =>
      t.addEventListener("click", () => {
        userPicked = true;
      })
    );
    const order = ["service", "browser", "schedule", "model"];
    let idx = 0;
    setInterval(() => {
      if (hovered || !onScreen || userPicked) return;
      idx = (idx + 1) % order.length;
      activate(order[idx]);
    }, 4800);
  }
})();
