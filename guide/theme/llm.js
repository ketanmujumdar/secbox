// Adds "Copy page" and "Markdown" buttons to every page, so its source can be
// pasted into an AI assistant. The Markdown files are written next to the HTML
// by tools/llms.py. No dependencies, no network calls except to this site.
(function () {
    "use strict";

    function markdownUrl() {
        var path = window.location.pathname;
        if (path.endsWith("/")) {
            path += "index.html";
        }
        return path.replace(/\.html$/, ".md");
    }

    function icon(pathData) {
        var ns = "http://www.w3.org/2000/svg";
        var svg = document.createElementNS(ns, "svg");
        svg.setAttribute("viewBox", "0 0 24 24");
        svg.setAttribute("aria-hidden", "true");
        var path = document.createElementNS(ns, "path");
        path.setAttribute("d", pathData);
        svg.appendChild(path);
        var span = document.createElement("span");
        span.className = "fa-svg llm-icon";
        span.appendChild(svg);
        return span;
    }

    function copyText(text) {
        if (navigator.clipboard && window.isSecureContext) {
            return navigator.clipboard.writeText(text);
        }
        return new Promise(function (resolve, reject) {
            var area = document.createElement("textarea");
            area.value = text;
            area.setAttribute("readonly", "");
            area.style.position = "fixed";
            area.style.opacity = "0";
            document.body.appendChild(area);
            area.select();
            var ok = document.execCommand("copy");
            document.body.removeChild(area);
            ok ? resolve() : reject(new Error("copy failed"));
        });
    }

    function flash(button, text) {
        var label = button.querySelector(".llm-label");
        var before = label.textContent;
        label.textContent = text;
        setTimeout(function () { label.textContent = before; }, 1600);
    }

    // Material Design icons (Apache-2.0): content-copy and language-markdown.
    var COPY = "M19 21H8V7h11m0-2H8a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2m-3-4H4a2 2 0 0 0-2 2v14h2V3h12V1z";
    var MD = "M20.56 18H3.44C2.65 18 2 17.37 2 16.59V7.41C2 6.63 2.65 6 3.44 6h17.12c.79 0 1.44.63 1.44 1.41v9.18c0 .78-.65 1.41-1.44 1.41M6.81 15.19v-3.66l1.92 2.35 1.92-2.35v3.66h1.93V8.81h-1.93l-1.92 2.35-1.92-2.35H4.89v6.38h1.92M19.69 12h-1.92V8.81h-1.92V12h-1.93l2.89 3.28L19.69 12z";

    function init() {
        var bar = document.querySelector("#mdbook-menu-bar .right-buttons, #menu-bar .right-buttons");
        if (!bar) {
            return;
        }
        var url = markdownUrl();

        var copy = document.createElement("button");
        copy.type = "button";
        copy.className = "llm-button";
        copy.title = "Copy this page as Markdown, for pasting into an AI assistant";
        copy.setAttribute("aria-label", copy.title);
        copy.appendChild(icon(COPY));
        var copyLabel = document.createElement("span");
        copyLabel.className = "llm-label";
        copyLabel.textContent = "Copy page";
        copy.appendChild(copyLabel);
        copy.addEventListener("click", function () {
            fetch(url, { credentials: "same-origin" })
                .then(function (r) {
                    if (!r.ok) { throw new Error(r.status); }
                    return r.text();
                })
                .then(copyText)
                .then(function () { flash(copy, "Copied"); })
                .catch(function () { flash(copy, "Copy failed"); });
        });

        var view = document.createElement("a");
        view.href = url;
        view.className = "llm-button";
        view.title = "Open this page as plain Markdown";
        view.setAttribute("aria-label", view.title);
        view.appendChild(icon(MD));
        var viewLabel = document.createElement("span");
        viewLabel.className = "llm-label";
        viewLabel.textContent = "Markdown";
        view.appendChild(viewLabel);

        bar.insertBefore(view, bar.firstChild);
        bar.insertBefore(copy, bar.firstChild);
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", init);
    } else {
        init();
    }
})();
