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

        // Draw connections
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
                    ctx.strokeStyle = `rgba(139, 92, 246, ${alpha})`;
                    ctx.lineWidth = 0.5;
                    ctx.stroke();
                }
            }
        }

        // Draw particles
        for (let i = 0; i < particles.length; i++) {
            const p = particles[i];

            // Mouse interaction
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

            // Damping
            p.vx *= 0.999;
            p.vy *= 0.999;

            // Wrap around edges
            if (p.x < 0) p.x = canvas.width;
            if (p.x > canvas.width) p.x = 0;
            if (p.y < 0) p.y = canvas.height;
            if (p.y > canvas.height) p.y = 0;

            ctx.beginPath();
            ctx.arc(p.x, p.y, p.radius, 0, Math.PI * 2);
            ctx.fillStyle = `rgba(139, 92, 246, ${p.opacity})`;
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

    // ===========================
    // Scroll Reveal (IntersectionObserver)
    // ===========================
    const revealElements = document.querySelectorAll('.reveal');

    const revealObserver = new IntersectionObserver(
        function (entries) {
            entries.forEach(function (entry) {
                if (entry.isIntersecting) {
                    // Stagger children in grids
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
                const navHeight = document.querySelector('nav').offsetHeight;
                const top = target.getBoundingClientRect().top + window.pageYOffset - navHeight - 20;
                window.scrollTo({ top: top, behavior: 'smooth' });
            }
            // Close mobile menu if open
            closeMobileMenu();
        });
    });

    // ===========================
    // Nav Background on Scroll
    // ===========================
    const nav = document.getElementById('nav');
    let lastScroll = 0;

    window.addEventListener('scroll', function () {
        const scrollY = window.scrollY;
        if (scrollY > 60) {
            nav.classList.add('scrolled');
        } else {
            nav.classList.remove('scrolled');
        }
        lastScroll = scrollY;
    }, { passive: true });

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
    // Code Block Copy Button
    // ===========================
    const copyBtn = document.getElementById('codeCopy');
    if (copyBtn) {
        copyBtn.addEventListener('click', function () {
            const codeBlock = document.querySelector('.code-body code');
            if (codeBlock) {
                const text = codeBlock.textContent;
                navigator.clipboard.writeText(text).then(function () {
                    copyBtn.textContent = 'Copied!';
                    copyBtn.classList.add('copied');
                    setTimeout(function () {
                        copyBtn.textContent = 'Copy';
                        copyBtn.classList.remove('copied');
                    }, 2000);
                }).catch(function () {
                    // Fallback for older browsers
                    const textarea = document.createElement('textarea');
                    textarea.value = text;
                    textarea.style.position = 'fixed';
                    textarea.style.opacity = '0';
                    document.body.appendChild(textarea);
                    textarea.select();
                    document.execCommand('copy');
                    document.body.removeChild(textarea);
                    copyBtn.textContent = 'Copied!';
                    copyBtn.classList.add('copied');
                    setTimeout(function () {
                        copyBtn.textContent = 'Copy';
                        copyBtn.classList.remove('copied');
                    }, 2000);
                });
            }
        });
    }

    // ===========================
    // Terminal Typing Animation
    // ===========================
    const terminalBody = document.getElementById('terminalBody');
    if (terminalBody) {
        const lines = terminalBody.querySelectorAll('.terminal-line');
        lines.forEach(function (line, i) {
            line.style.opacity = '0';
            line.style.transform = 'translateY(4px)';
            line.style.transition = 'opacity 0.4s ease, transform 0.4s ease';
        });

        // Observer to trigger when terminal enters view
        const terminalObserver = new IntersectionObserver(
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

        terminalObserver.observe(terminalBody.closest('.terminal'));
    }

})();
