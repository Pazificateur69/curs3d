(function () {
    "use strict";

    var STORAGE_KEY = "curs3d-lang";
    var nav = document.getElementById("nav");
    var navToggle = document.getElementById("navToggle");
    var mobileMenu = document.getElementById("mobileMenu");

    function getPreferredLanguage() {
        var saved = window.localStorage.getItem(STORAGE_KEY);
        if (saved === "fr" || saved === "en") {
            return saved;
        }
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

        if (title) {
            document.title = title;
        }
        if (description && metaDescription) {
            metaDescription.setAttribute("content", description);
        }

        document.querySelectorAll("[data-placeholder-en]").forEach(function (field) {
            var value = field.getAttribute("data-placeholder-" + selected);
            if (value) {
                field.setAttribute("placeholder", value);
            }
        });
    }

    function closeMobileMenu() {
        if (!navToggle || !mobileMenu) {
            return;
        }
        navToggle.classList.remove("active");
        mobileMenu.classList.remove("open");
    }

    function syncNavState() {
        if (!nav) {
            return;
        }
        if (window.scrollY > 12) {
            nav.classList.add("scrolled");
        } else {
            nav.classList.remove("scrolled");
        }
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
            { threshold: 0.1, rootMargin: "0px 0px -48px 0px" }
        );
        revealElements.forEach(function (element) {
            observer.observe(element);
        });
    } else {
        revealElements.forEach(function (element) {
            element.classList.add("visible");
        });
    }

    document.querySelectorAll(".code-copy").forEach(function (button) {
        button.addEventListener("click", function () {
            var block = button.closest(".code-block");
            if (!block) {
                return;
            }
            var code = block.querySelector(".code-body");
            if (!code) {
                return;
            }
            navigator.clipboard.writeText(code.textContent || "").then(function () {
                var original = button.textContent;
                button.textContent = "Copied";
                setTimeout(function () {
                    button.textContent = original;
                }, 1200);
            });
        });
    });

    var yearElements = document.querySelectorAll("[data-year]");
    yearElements.forEach(function (element) {
        element.textContent = String(new Date().getFullYear());
    });

    var tocLinks = document.querySelectorAll("[data-toc-link]");
    if (tocLinks.length > 0 && "IntersectionObserver" in window) {
        var sections = Array.from(document.querySelectorAll("section[id]"));
        var tocObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (!entry.isIntersecting) {
                        return;
                    }
                    tocLinks.forEach(function (link) {
                        link.classList.toggle(
                            "active",
                            link.getAttribute("href") === "#" + entry.target.id
                        );
                    });
                });
            },
            { threshold: 0.2, rootMargin: "-20% 0px -60% 0px" }
        );

        sections.forEach(function (section) {
            tocObserver.observe(section);
        });
    }

    setLanguage(getPreferredLanguage());
})();
