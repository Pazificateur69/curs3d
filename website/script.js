/* ============================================
   CURS3D - Quantum-Resistant Blockchain
   Main JavaScript v4.0
   ============================================ */

(function () {
    'use strict';

    // ===========================
    // Particle Animation System
    // ===========================
    var canvas = document.getElementById('particles');
    if (canvas) {
        var ctx = canvas.getContext('2d');
        var particles = [];
        var animationId;
        var mouseX = -1000;
        var mouseY = -1000;

        function resizeCanvas() {
            canvas.width = window.innerWidth;
            canvas.height = window.innerHeight;
        }

        function createParticles() {
            particles = [];
            var count = Math.min(90, Math.floor((window.innerWidth * window.innerHeight) / 14000));
            for (var i = 0; i < count; i++) {
                var hue = Math.random() > 0.6 ? 190 : Math.random() > 0.3 ? 270 : 330;
                particles.push({
                    x: Math.random() * canvas.width,
                    y: Math.random() * canvas.height,
                    vx: (Math.random() - 0.5) * 0.25,
                    vy: (Math.random() - 0.5) * 0.25,
                    radius: Math.random() * 1.5 + 0.5,
                    opacity: Math.random() * 0.5 + 0.1,
                    hue: hue,
                });
            }
        }

        function drawParticles() {
            ctx.clearRect(0, 0, canvas.width, canvas.height);

            for (var i = 0; i < particles.length; i++) {
                for (var j = i + 1; j < particles.length; j++) {
                    var dx = particles[i].x - particles[j].x;
                    var dy = particles[i].y - particles[j].y;
                    var dist = Math.sqrt(dx * dx + dy * dy);

                    if (dist < 140) {
                        var alpha = (1 - dist / 140) * 0.1;
                        ctx.beginPath();
                        ctx.moveTo(particles[i].x, particles[i].y);
                        ctx.lineTo(particles[j].x, particles[j].y);
                        ctx.strokeStyle = 'rgba(139, 92, 246, ' + alpha + ')';
                        ctx.lineWidth = 0.5;
                        ctx.stroke();
                    }
                }
            }

            for (var k = 0; k < particles.length; k++) {
                var p = particles[k];

                var dx2 = mouseX - p.x;
                var dy2 = mouseY - p.y;
                var dist2 = Math.sqrt(dx2 * dx2 + dy2 * dy2);
                if (dist2 < 200) {
                    var force = (200 - dist2) / 200;
                    p.vx -= (dx2 / dist2) * force * 0.015;
                    p.vy -= (dy2 / dist2) * force * 0.015;
                }

                p.x += p.vx;
                p.y += p.vy;
                p.vx *= 0.999;
                p.vy *= 0.999;

                if (p.x < 0) p.x = canvas.width;
                if (p.x > canvas.width) p.x = 0;
                if (p.y < 0) p.y = canvas.height;
                if (p.y > canvas.height) p.y = 0;

                ctx.beginPath();
                ctx.arc(p.x, p.y, p.radius, 0, Math.PI * 2);
                ctx.fillStyle = 'hsla(' + p.hue + ', 70%, 60%, ' + p.opacity + ')';
                ctx.fill();
            }

            animationId = requestAnimationFrame(drawParticles);
        }

        resizeCanvas();
        createParticles();
        drawParticles();

        var resizeTimer;
        window.addEventListener('resize', function () {
            clearTimeout(resizeTimer);
            resizeTimer = setTimeout(function () {
                resizeCanvas();
                createParticles();
            }, 150);
        });

        document.addEventListener('mousemove', function (e) {
            mouseX = e.clientX;
            mouseY = e.clientY;
        });
    }

    // ===========================
    // Scroll Reveal (IntersectionObserver)
    // ===========================
    var revealElements = document.querySelectorAll('.reveal');

    if (revealElements.length > 0) {
        var revealObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (entry.isIntersecting) {
                        var parent = entry.target.parentElement;
                        if (parent) {
                            var siblings = Array.from(parent.querySelectorAll('.reveal'));
                            var index = siblings.indexOf(entry.target);
                            if (index === -1) index = 0;
                            entry.target.style.transitionDelay = (index * 0.07) + 's';
                        }
                        entry.target.classList.add('visible');
                        revealObserver.unobserve(entry.target);
                    }
                });
            },
            { threshold: 0.08, rootMargin: '0px 0px -50px 0px' }
        );

        revealElements.forEach(function (el) {
            revealObserver.observe(el);
        });
    }

    // ===========================
    // Smooth Scroll for Anchor Links
    // ===========================
    document.querySelectorAll('a[href^="#"]').forEach(function (anchor) {
        anchor.addEventListener('click', function (e) {
            var href = this.getAttribute('href');
            if (href === '#') return;
            e.preventDefault();
            var target = document.querySelector(href);
            if (target) {
                var navEl = document.querySelector('nav');
                var navHeight = navEl ? navEl.offsetHeight : 72;
                var top = target.getBoundingClientRect().top + window.pageYOffset - navHeight - 20;
                window.scrollTo({ top: top, behavior: 'smooth' });
            }
            closeMobileMenu();
        });
    });

    // ===========================
    // Nav Background on Scroll
    // ===========================
    var nav = document.getElementById('nav');

    if (nav) {
        window.addEventListener('scroll', function () {
            if (window.scrollY > 60) {
                nav.classList.add('scrolled');
            } else {
                nav.classList.remove('scrolled');
            }
        }, { passive: true });
    }

    // ===========================
    // Mobile Hamburger Menu
    // ===========================
    var hamburger = document.getElementById('hamburger');
    var mobileMenu = document.getElementById('mobileMenu');

    function closeMobileMenu() {
        if (hamburger && mobileMenu) {
            hamburger.classList.remove('active');
            mobileMenu.classList.remove('open');
        }
    }

    if (hamburger && mobileMenu) {
        hamburger.addEventListener('click', function () {
            hamburger.classList.toggle('active');
            mobileMenu.classList.toggle('open');
        });

        mobileMenu.querySelectorAll('a').forEach(function (link) {
            link.addEventListener('click', closeMobileMenu);
        });

        document.addEventListener('click', function (e) {
            if (!mobileMenu.contains(e.target) && !hamburger.contains(e.target)) {
                closeMobileMenu();
            }
        });
    }

    // ===========================
    // Code Block Copy Buttons
    // ===========================
    document.querySelectorAll('.code-copy').forEach(function (btn) {
        btn.addEventListener('click', function () {
            var block = btn.closest('.code-block, .code-block-full');
            if (!block) return;
            var codeEl = block.querySelector('.code-body code') || block.querySelector('.code-body');
            if (!codeEl) return;
            var text = codeEl.textContent;
            navigator.clipboard.writeText(text).then(function () {
                btn.textContent = 'Copied!';
                btn.classList.add('copied');
                setTimeout(function () {
                    btn.textContent = 'Copy';
                    btn.classList.remove('copied');
                }, 2000);
            }).catch(function () {
                var textarea = document.createElement('textarea');
                textarea.value = text;
                textarea.style.position = 'fixed';
                textarea.style.opacity = '0';
                document.body.appendChild(textarea);
                textarea.select();
                document.execCommand('copy');
                document.body.removeChild(textarea);
                btn.textContent = 'Copied!';
                btn.classList.add('copied');
                setTimeout(function () {
                    btn.textContent = 'Copy';
                    btn.classList.remove('copied');
                }, 2000);
            });
        });
    });

    // ===========================
    // Terminal Typing Animation
    // ===========================
    var terminalBody = document.getElementById('terminalBody');
    if (terminalBody) {
        var lines = terminalBody.querySelectorAll('.terminal-line');
        lines.forEach(function (line) {
            line.style.opacity = '0';
            line.style.transform = 'translateY(4px)';
            line.style.transition = 'opacity 0.4s ease, transform 0.4s ease';
        });

        var terminalObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (entry.isIntersecting) {
                        lines.forEach(function (line, i) {
                            setTimeout(function () {
                                line.style.opacity = '1';
                                line.style.transform = 'translateY(0)';
                            }, i * 160);
                        });
                        terminalObserver.unobserve(entry.target);
                    }
                });
            },
            { threshold: 0.2 }
        );

        var terminalEl = terminalBody.closest('.terminal');
        if (terminalEl) {
            terminalObserver.observe(terminalEl);
        }
    }

    // ===========================
    // Docs Sidebar Active Tracking
    // ===========================
    var sidebarLinks = document.querySelectorAll('.sidebar-link');
    var docsSections = document.querySelectorAll('.docs-section');

    if (sidebarLinks.length > 0 && docsSections.length > 0) {
        var sectionObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (entry.isIntersecting) {
                        var id = entry.target.getAttribute('id');
                        sidebarLinks.forEach(function (link) {
                            link.classList.remove('active');
                            if (link.getAttribute('href') === '#' + id) {
                                link.classList.add('active');
                            }
                        });
                    }
                });
            },
            { threshold: 0.15, rootMargin: '-80px 0px -60% 0px' }
        );

        docsSections.forEach(function (section) {
            sectionObserver.observe(section);
        });
    }

    // ===========================
    // Counter Animation
    // ===========================
    var counterElements = document.querySelectorAll('[data-count]');
    if (counterElements.length > 0) {
        var counterObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (entry.isIntersecting) {
                        var el = entry.target;
                        var target = parseInt(el.getAttribute('data-count'), 10);
                        var suffix = el.getAttribute('data-suffix') || '';
                        var duration = 1500;
                        var start = 0;
                        var startTime = null;

                        function animate(ts) {
                            if (!startTime) startTime = ts;
                            var progress = Math.min((ts - startTime) / duration, 1);
                            var eased = 1 - Math.pow(1 - progress, 3);
                            var current = Math.floor(eased * target);
                            el.textContent = current.toLocaleString() + suffix;
                            if (progress < 1) {
                                requestAnimationFrame(animate);
                            } else {
                                el.textContent = target.toLocaleString() + suffix;
                            }
                        }

                        requestAnimationFrame(animate);
                        counterObserver.unobserve(el);
                    }
                });
            },
            { threshold: 0.5 }
        );

        counterElements.forEach(function (el) {
            counterObserver.observe(el);
        });
    }

})();
