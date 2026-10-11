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

const isYouTubeChannelUrl = (value) => {
  try {
    const url = new URL(value);
    const isYouTube =
      url.hostname === "youtube.com" || url.hostname.endsWith(".youtube.com");
    if (!isYouTube || !["http:", "https:"].includes(url.protocol)) return false;

    const [first, second] = url.pathname.split("/").filter(Boolean);
    return (
      (first?.startsWith("@") && first.length > 1) ||
      (["channel", "c", "user"].includes(first) && Boolean(second))
    );
  } catch {
    return false;
  }
};

const replaceYouTubeChannelUrl = async (form, button, message) => {
  const urlInput = form.elements.namedItem("url");
  const csrfInput = form.elements.namedItem("csrf_token");
  if (
    !(urlInput instanceof HTMLInputElement) ||
    !(csrfInput instanceof HTMLInputElement) ||
    !isYouTubeChannelUrl(urlInput.value) ||
    !window.confirm(message)
  ) {
    return false;
  }

  const originalLabel = button?.textContent;
  if (button instanceof HTMLButtonElement) {
    button.disabled = true;
    button.textContent = "Finding feed...";
  }

  try {
    const body = new URLSearchParams({
      url: urlInput.value,
      csrf_token: csrfInput.value,
    });
    const response = await window.fetch("/feeds/youtube-feed", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body,
    });
    const result = await response.text();
    if (!response.ok) throw new Error(result || "The request failed.");

    urlInput.value = result;
    urlInput.focus();
    urlInput.setSelectionRange(0, 0);
    return true;
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : "The request failed.";
    window.alert(errorMessage);
    return false;
  } finally {
    if (button instanceof HTMLButtonElement) {
      button.disabled = false;
      button.textContent = originalLabel;
    }
  }
};

document.addEventListener(
  "submit",
  async (event) => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || form.id !== "add-feed-form") return;
    if (event.submitter?.getAttribute("formaction") === "/feeds/title") return;

    const urlInput = form.elements.namedItem("url");
    if (!(urlInput instanceof HTMLInputElement) || !isYouTubeChannelUrl(urlInput.value)) return;

    event.preventDefault();
    event.stopImmediatePropagation();
    const addButton = form.querySelector("#add-feed-button");
    await replaceYouTubeChannelUrl(
      form,
      addButton,
      "This is a YouTube channel page rather than a feed. Should Blatt try to find and use its RSS feed instead?\n\nChoose Cancel to keep editing.",
    );
  },
  true,
);

document.addEventListener(
  "click",
  async (event) => {
    if (!(event.target instanceof Element)) return;
    const button = event.target.closest('button[formaction="/feeds/title"]');
    if (!(button instanceof HTMLButtonElement) || !(button.form instanceof HTMLFormElement)) {
      return;
    }

    const urlInput = button.form.elements.namedItem("url");
    if (!(urlInput instanceof HTMLInputElement) || !isYouTubeChannelUrl(urlInput.value)) return;

    event.preventDefault();
    event.stopImmediatePropagation();
    const replaced = await replaceYouTubeChannelUrl(
      button.form,
      button,
      "Fetching the title requires a feed URL. Should Blatt find and use this YouTube channel's RSS feed first?\n\nChoose Cancel to keep editing.",
    );
    if (replaced) button.click();
  },
  true,
);

document.addEventListener("click", showArticleAsRead);
document.addEventListener("auxclick", (event) => {
  if (event.button === 1) showArticleAsRead(event);
});
