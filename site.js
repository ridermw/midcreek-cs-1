(() => {
  const clamp = (value, minimum, maximum) =>
    Math.min(maximum, Math.max(minimum, value));

  for (const control of document.querySelectorAll("[data-compare-control]")) {
    const comparison = control.closest("[data-comparison]");
    if (!comparison) continue;

    const update = () => {
      const fraction = clamp(Number(control.value) / 100, 0, 1);
      comparison.style.setProperty("--comparison", `${fraction * 100}%`);
      control.setAttribute("aria-valuenow", String(Math.round(fraction * 100)));
    };

    control.addEventListener("input", update);
    control.addEventListener("change", update);
    update();
  }

  const navLinks = [...document.querySelectorAll(".rail a[href^='#']")];
  const sections = navLinks
    .map((link) => document.querySelector(link.getAttribute("href")))
    .filter(Boolean);

  if ("IntersectionObserver" in window) {
    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((entry) => entry.isIntersecting)
          .sort(
            (left, right) => right.intersectionRatio - left.intersectionRatio,
          )[0];
        if (!visible) return;
        for (const link of navLinks) {
          const current = link.getAttribute("href") === `#${visible.target.id}`;
          if (current) link.setAttribute("aria-current", "true");
          else link.removeAttribute("aria-current");
        }
      },
      { rootMargin: "-20% 0px -65% 0px", threshold: [0.05, 0.25, 0.5] },
    );
    sections.forEach((section) => observer.observe(section));
  }
})();
