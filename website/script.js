/* ============================================
   CURS3D - Quantum-Resistant Blockchain
   Main JavaScript
   ============================================ */

(function () {
    'use strict';

    // ===========================
    // Particle Animation System
    // ===========================
    const canvas = document.getElementById('particles');
    if (canvas) {
        const ctx = canvas.getContext('2d');
        let particles = [];
        let animationId;
        let mouseX = -1000;
        let mouseY = -1000;

        function resizeCanvas() {
            canvas.width = window.innerWidth;
            canvas.height = window.innerHeight;
        }

        function createParticles() {
            particles = [];
            const count = Math.min(80, Math.floor((window.innerWidth * window.innerHeight) / 15000));
            for (let i = 0; i < count; i++) {
                particles.push({
                    x: Math.random() * canvas.width,
                    y: Math.random() * canvas.height,
                    vx: (Math.random() - 0.5) * 0.3,
                    vy: (Math.random() - 0.5) * 0.3,
                    radius: Math.random() * 1.5 + 0.5,
                    opacity: Math.random() * 0.5 + 0.1,
                });
            }
        }

        function drawParticles() {
            ctx.clearRect(0, 0, canvas.width, canvas.height);

            for (let i = 0; i < particles.length; i++) {
                for (let j = i + 1; j < particles.length; j++) {
                    const dx = particles[i].x - particles[j].x;
                    const dy = particles[i].y - particles[j].y;
                    const dist = Math.sqrt(dx * dx + dy * dy);

                    if (dist < 150) {
                        const alpha = (1 - dist / 150) * 0.12;
                        ctx.beginPath();
                        ctx.moveTo(particles[i].x, particles[i].y);
                        ctx.lineTo(particles[j].x, particles[j].y);
                        ctx.strokeStyle = 'rgba(139, 92, 246, ' + alpha + ')';
                        ctx.lineWidth = 0.5;
                        ctx.stroke();
                    }
                }
            }

            for (let i = 0; i < particles.length; i++) {
                const p = particles[i];

                const dx = mouseX - p.x;
                const dy = mouseY - p.y;
                const dist = Math.sqrt(dx * dx + dy * dy);
                if (dist < 200) {
                    const force = (200 - dist) / 200;
                    p.vx -= (dx / dist) * force * 0.02;
                    p.vy -= (dy / dist) * force * 0.02;
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
                ctx.fillStyle = 'rgba(139, 92, 246, ' + p.opacity + ')';
                ctx.fill();
            }

            animationId = requestAnimationFrame(drawParticles);
        }

        resizeCanvas();
        createParticles();
        drawParticles();

        window.addEventListener('resize', function () {
            resizeCanvas();
            createParticles();
        });

        document.addEventListener('mousemove', function (e) {
            mouseX = e.clientX;
            mouseY = e.clientY;
        });
    }

    // ===========================
    // Scroll Reveal (IntersectionObserver)
    // ===========================
    const revealElements = document.querySelectorAll('.reveal');

    if (revealElements.length > 0) {
        const revealObserver = new IntersectionObserver(
            function (entries) {
                entries.forEach(function (entry) {
                    if (entry.isIntersecting) {
                        const parent = entry.target.parentElement;
                        if (parent) {
                            const siblings = parent.querySelectorAll('.reveal');
                            let index = 0;
                            siblings.forEach(function (sib, i) {
                                if (sib === entry.target) index = i;
                            });
                            entry.target.style.transitionDelay = (index * 0.08) + 's';
                        }
                        entry.target.classList.add('visible');
                        revealObserver.unobserve(entry.target);
                    }
                });
            },
            { threshold: 0.1, rootMargin: '0px 0px -40px 0px' }
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
            const href = this.getAttribute('href');
            if (href === '#') return;
            e.preventDefault();
            const target = document.querySelector(href);
            if (target) {
                const navEl = document.querySelector('nav');
                const navHeight = navEl ? navEl.offsetHeight : 72;
                const top = target.getBoundingClientRect().top + window.pageYOffset - navHeight - 20;
                window.scrollTo({ top: top, behavior: 'smooth' });
            }
            closeMobileMenu();
        });
    });

    // ===========================
    // Nav Background on Scroll
    // ===========================
    const nav = document.getElementById('nav');

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
    const hamburger = document.getElementById('hamburger');
    const mobileMenu = document.getElementById('mobileMenu');

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
    // Code Block Copy Buttons (all pages)
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
                            }, i * 200);
                        });
                        terminalObserver.unobserve(entry.target);
                    }
                });
            },
            { threshold: 0.3 }
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

})();
