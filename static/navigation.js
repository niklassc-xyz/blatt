const refreshArticles = () => {
  const panel = document.querySelector("#articles-panel");
  if (!panel) return;

  if (window.htmx) {
    window.htmx.ajax("GET", `/articles${window.location.search}`, {
      target: panel,
      swap: "innerHTML",
    });
  } else {
    window.location.reload();
  }
};

window.addEventListener("pageshow", (event) => {
  if (event.persisted) refreshArticles();
});

let wasHidden = false;
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    wasHidden = true;
  } else if (wasHidden) {
    wasHidden = false;
    refreshArticles();
  }
});

const showArticleAsRead = (event) => {
  if (!(event.target instanceof Element)) return;
  const link = event.target.closest("[data-article-open]");
  if (!link) return;

  const row = link.closest(".article-row");
  if (!row) return;
  window.setTimeout(() => {
    if (new URLSearchParams(window.location.search).get("status") === "unread") {
      row.remove();
      return;
    }

    row.classList.add("is-read");
    const action = row.querySelector('input[name="action"]');
    const button = row.querySelector(".read-toggle");
    if (action) action.value = "unread";
    if (button) button.textContent = "Mark unread";
  }, 0);
};

document.addEventListener("click", showArticleAsRead);
document.addEventListener("auxclick", (event) => {
  if (event.button === 1) showArticleAsRead(event);
});
