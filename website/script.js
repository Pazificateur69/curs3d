(function () {
    "use strict";

    /* ─── Language System ────────────────────────────────────────── */

    var STORAGE_KEY = "curs3d-lang";
    var nav = document.getElementById("nav");
    var navToggle = document.getElementById("navToggle");
    var mobileMenu = document.getElementById("mobileMenu");

    function getPreferredLanguage() {
        var saved = window.localStorage.getItem(STORAGE_KEY);
        if (saved === "fr" || saved === "en") return saved;
        var lang = (navigator.language || "en").toLowerCase();
        return lang.indexOf("fr") === 0 ? "fr" : "en";
    }

    function setLanguage(lang) {
        var selected = lang === "fr" ? "fr" : "en";
        document.documentElement.setAttribute("data-current-lang", selected);
        document.documentElement.setAttribute("lang", selected);
        window.localStorage.setItem(STORAGE_KEY, selected);

        document.querySelectorAll("[data-lang-switch]").forEach(function (button) {
            button.classList.toggle("active", button.getAttribute("data-lang-switch") === selected);
        });

        var title = document.body.getAttribute("data-title-" + selected);
        var description = document.body.getAttribute("data-description-" + selected);
        var metaDescription = document.querySelector('meta[name="description"]');
        if (title) document.title = title;
        if (description && metaDescription) metaDescription.setAttribute("content", description);

        document.querySelectorAll("[data-placeholder-en]").forEach(function (field) {
            var value = field.getAttribute("data-placeholder-" + selected);
            if (value) field.setAttribute("placeholder", value);
        });
    }

    /* ─── Navigation ─────────────────────────────────────────────── */

    function closeMobileMenu() {
        if (!navToggle || !mobileMenu) return;
        navToggle.classList.remove("active");
        mobileMenu.classList.remove("open");
    }

    function syncNavState() {
        if (!nav) return;
        nav.classList.toggle("scrolled", window.scrollY > 12);
    }

    document.querySelectorAll("[data-lang-switch]").forEach(function (button) {
        button.addEventListener("click", function () {
            setLanguage(button.getAttribute("data-lang-switch"));
        });
    });

    if (navToggle && mobileMenu) {
        navToggle.addEventListener("click", function () {
            navToggle.classList.toggle("active");
            mobileMenu.classList.toggle("open");
        });
        mobileMenu.querySelectorAll("a").forEach(function (link) {
            link.addEventListener("click", closeMobileMenu);
        });
    }

    window.addEventListener("scroll", syncNavState, { passive: true });
    syncNavState();

    /* ─── Scroll Reveal ──────────────────────────────────────────── */

    var revealElements = document.querySelectorAll(".reveal");
    if ("IntersectionObserver" in window && revealElements.length > 0) {
        var observer = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (entry.isIntersecting) {
                        entry.target.classList.add("visible");
                        observer.unobserve(entry.target);
                    }
                });
            },
            { threshold: 0.08, rootMargin: "0px 0px -40px 0px" }
        );
        revealElements.forEach(function (element) { observer.observe(element); });
    } else {
        revealElements.forEach(function (element) { element.classList.add("visible"); });
    }

    /* ─── Code Copy ──────────────────────────────────────────────── */

    document.querySelectorAll(".code-copy").forEach(function (button) {
        button.addEventListener("click", function () {
            var block = button.closest(".code-block");
            if (!block) return;
            var code = block.querySelector(".code-body");
            if (!code) return;
            navigator.clipboard.writeText(code.textContent || "").then(function () {
                var original = button.textContent;
                button.textContent = "Copied!";
                button.style.borderColor = "rgba(63, 208, 212, 0.5)";
                setTimeout(function () {
                    button.textContent = original;
                    button.style.borderColor = "";
                }, 1400);
            });
        });
    });

    /* ─── Year ───────────────────────────────────────────────────── */

    document.querySelectorAll("[data-year]").forEach(function (element) {
        element.textContent = String(new Date().getFullYear());
    });

    /* ─── TOC Active Tracking ────────────────────────────────────── */

    var tocLinks = document.querySelectorAll("[data-toc-link]");
    if (tocLinks.length > 0 && "IntersectionObserver" in window) {
        var sections = Array.from(document.querySelectorAll("section[id]"));
        var tocObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (!entry.isIntersecting) return;
                    tocLinks.forEach(function (link) {
                        link.classList.toggle("active", link.getAttribute("href") === "#" + entry.target.id);
                    });
                });
            },
            { threshold: 0.2, rootMargin: "-20% 0px -60% 0px" }
        );
        sections.forEach(function (section) { tocObserver.observe(section); });
    }

    /* ─── Particle System ────────────────────────────────────────── */

    function initParticles() {
        var canvas = document.getElementById("particles-canvas");
        if (!canvas) {
            canvas = document.createElement("canvas");
            canvas.id = "particles-canvas";
            document.body.prepend(canvas);
        }

        var ctx = canvas.getContext("2d");
        var particles = [];
        var mouse = { x: -9999, y: -9999 };
        var PARTICLE_COUNT = 60;
        var CONNECTION_DISTANCE = 140;
        var dpr = window.devicePixelRatio || 1;

        function resize() {
            canvas.width = window.innerWidth * dpr;
            canvas.height = window.innerHeight * dpr;
            canvas.style.width = window.innerWidth + "px";
            canvas.style.height = window.innerHeight + "px";
            ctx.scale(dpr, dpr);
        }

        function createParticle() {
            return {
                x: Math.random() * window.innerWidth,
                y: Math.random() * window.innerHeight,
                vx: (Math.random() - 0.5) * 0.3,
                vy: (Math.random() - 0.5) * 0.3,
                size: Math.random() * 1.5 + 0.5,
                opacity: Math.random() * 0.4 + 0.1
            };
        }

        resize();
        for (var i = 0; i < PARTICLE_COUNT; i++) particles.push(createParticle());

        function animate() {
            ctx.clearRect(0, 0, window.innerWidth, window.innerHeight);

            for (var i = 0; i < particles.length; i++) {
                var p = particles[i];
                p.x += p.vx;
                p.y += p.vy;

                if (p.x < 0 || p.x > window.innerWidth) p.vx *= -1;
                if (p.y < 0 || p.y > window.innerHeight) p.vy *= -1;

                // Draw particle
                ctx.beginPath();
                ctx.arc(p.x, p.y, p.size, 0, Math.PI * 2);
                ctx.fillStyle = "rgba(63, 208, 212, " + p.opacity + ")";
                ctx.fill();

                // Draw connections
                for (var j = i + 1; j < particles.length; j++) {
                    var p2 = particles[j];
                    var dx = p.x - p2.x;
                    var dy = p.y - p2.y;
                    var dist = Math.sqrt(dx * dx + dy * dy);

                    if (dist < CONNECTION_DISTANCE) {
                        var alpha = (1 - dist / CONNECTION_DISTANCE) * 0.12;
                        ctx.beginPath();
                        ctx.moveTo(p.x, p.y);
                        ctx.lineTo(p2.x, p2.y);
                        ctx.strokeStyle = "rgba(63, 208, 212, " + alpha + ")";
                        ctx.lineWidth = 0.5;
                        ctx.stroke();
                    }
                }

                // Mouse interaction
                var mx = p.x - mouse.x;
                var my = p.y - mouse.y;
                var mDist = Math.sqrt(mx * mx + my * my);
                if (mDist < 180) {
                    ctx.beginPath();
                    ctx.moveTo(p.x, p.y);
                    ctx.lineTo(mouse.x, mouse.y);
                    ctx.strokeStyle = "rgba(63, 208, 212, " + ((1 - mDist / 180) * 0.15) + ")";
                    ctx.lineWidth = 0.6;
                    ctx.stroke();
                }
            }

            requestAnimationFrame(animate);
        }

        document.addEventListener("mousemove", function (e) {
            mouse.x = e.clientX;
            mouse.y = e.clientY;
        });

        window.addEventListener("resize", resize);
        animate();
    }

    // Only init particles on desktop (perf)
    if (window.innerWidth > 768) {
        initParticles();
    }

    /* ─── Count-Up Animation for Metrics ─────────────────────────── */

    function animateCountUp(element, target, duration) {
        var start = 0;
        var startTime = null;
        var original = element.textContent;

        function step(timestamp) {
            if (!startTime) startTime = timestamp;
            var progress = Math.min((timestamp - startTime) / duration, 1);
            var eased = 1 - Math.pow(1 - progress, 3); // ease-out cubic
            var current = Math.floor(eased * target);
            element.textContent = current;
            if (progress < 1) requestAnimationFrame(step);
            else element.textContent = original; // restore original (may have text suffix)
        }

        requestAnimationFrame(step);
    }

    var metricElements = document.querySelectorAll(".metric strong");
    if ("IntersectionObserver" in window && metricElements.length > 0) {
        var metricObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (!entry.isIntersecting) return;
                    var el = entry.target;
                    var text = el.textContent.trim();
                    var num = parseInt(text, 10);
                    if (!isNaN(num) && num > 0) {
                        animateCountUp(el, num, 1200);
                    }
                    metricObserver.unobserve(el);
                });
            },
            { threshold: 0.5 }
        );
        metricElements.forEach(function (el) { metricObserver.observe(el); });
    }

    /* ─── Init Language ──────────────────────────────────────────── */

    setLanguage(getPreferredLanguage());
})();
