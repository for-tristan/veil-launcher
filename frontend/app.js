/* ─────────────────────────────────────────────────────────────
   Veil — app.js
   Vanilla JS controller. Detects Tauri at runtime; if running
   outside Tauri (browser preview), falls back to a JS mock backend
   so the UI is fully interactive without Rust.
   ───────────────────────────────────────────────────────────── */

(() => {
  "use strict";

  /* ─────────── Tauri / mock backend bridge ─────────── */

  const isTauri = Boolean(window.__TAURI_INTERNALS__ || window.__TAURI__);
  const invoke = isTauri
    ? window.__TAURI__.core.invoke
    : null;

  // Simulated latency for the mock backend — keeps the UI feeling real
  // without being instant/jarring.
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));

  // In-memory "installed" set; persisted to localStorage in browser mode.
  const installedSet = new Set();

  function loadPersisted() {
    try {
      const saved = JSON.parse(localStorage.getItem("veil.installed") || "[]");
      saved.forEach((id) => installedSet.add(id));
    } catch (_) {}
  }
  function persist() {
    if (!isTauri) {
      localStorage.setItem("veil.installed", JSON.stringify([...installedSet]));
    }
  }

  // Mock backend implementation (browser preview path).
  // Mirrors the Tauri command names so app logic is identical.
  const mockBackend = {
    async get_lol_path() {
      await wait(600);
      return window.VEIL_DATA.lolPath;
    },
    async get_champions() {
      await wait(120);
      return window.VEIL_DATA.champions;
    },
    async install_skin(_skinId) {
      await wait(700 + Math.random() * 500);
      return { ok: true, message: "Skin injected successfully." };
    },
    async uninstall_skin(_skinId) {
      await wait(500 + Math.random() * 300);
      return { ok: true, message: "Skin removed." };
    },
    async restore_all() {
      await wait(1200);
      return { ok: true, message: "All skins restored to original." };
    },
    async add_local_skin(_filePath) {
      await wait(900);
      return { ok: true, message: "Skin added to library." };
    },
    async add_url_skin(_url) {
      await wait(1100);
      // Simulate occasional failure for realism
      if (Math.random() < 0.08) {
        return { ok: false, message: "Download failed (404)." };
      }
      return { ok: true, message: "Skin downloaded & added." };
    },
    get_installed() {
      return [...installedSet];
    },
  };

  async function backend(name, ...args) {
    if (isTauri && invoke) {
      return invoke(name, args[0] || {});
    }
    if (mockBackend[name]) return mockBackend[name](...args);
    throw new Error(`Unknown backend command: ${name}`);
  }

  /* ─────────── App state ─────────── */

  const state = {
    champions: [],
    selectedChampId: null,
    searchQuery: "",
    roleFilter: "all",
    source: "local", // 'local' | 'repo'
    lolPath: null,
  };

  /* ─────────── DOM helpers ─────────── */

  const $ = (sel) => document.querySelector(sel);
  const $$ = (sel) => [...document.querySelectorAll(sel)];

  const el = {
    championList: $("#champion-list"),
    championCount: $("#champion-count"),
    championEmpty: $("#champion-empty"),
    roleFilter: $("#role-filter"),
    searchInput: $("#search-input"),
    searchClear: $("#search-clear"),
    searchKbd: $("#search-kbd"),
    contentChampName: $("#content-champ-name"),
    contentChampMeta: $("#content-champ-meta"),
    skinGrid: $("#skin-grid"),
    skinEmpty: $("#skin-empty"),
    installedCount: $("#installed-count"),
    installedSummary: $("#installed-summary"),
    uninstallAll: $("#uninstall-all"),
    lolStatusDot: $("#lol-status-dot"),
    lolStatusText: $("#lol-status-text"),
    appVersion: $("#app-version"),
    sourceLocal: $("#source-local"),
    sourceRepo: $("#source-repo"),
    addSkinBtn: $("#add-skin-btn"),
    settingsBtn: $("#settings-btn"),
    addModal: $("#add-modal"),
    dropzone: $("#dropzone"),
    fileInput: $("#file-input"),
    fileInfo: $("#file-info"),
    fileName: $("#file-name"),
    fileMeta: $("#file-meta"),
    fileClear: $("#file-clear"),
    urlInput: $("#url-input"),
    addConfirm: $("#add-confirm"),
    toast: $("#toast"),
  };

  function fmtBytes(n) {
    if (n < 1024) return n + " B";
    if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
    return (n / (1024 * 1024)).toFixed(1) + " MB";
  }

  function debounce(fn, ms) {
    let t;
    return (...args) => {
      clearTimeout(t);
      t = setTimeout(() => fn(...args), ms);
    };
  }

  /* ─────────── Toast ─────────── */

  let toastTimer;
  function toast(message, kind = "default") {
    el.toast.textContent = message;
    el.toast.className = "toast " + (kind === "ok" ? "ok" : kind === "err" ? "err" : "");
    el.toast.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (el.toast.hidden = true), 2600);
  }

  /* ─────────── Rendering ─────────── */

  function matchesChampion(champ) {
    if (state.roleFilter !== "all" && champ.role !== state.roleFilter) return false;
    if (state.searchQuery) {
      const q = state.searchQuery.toLowerCase();
      const nameHit = champ.name.toLowerCase().includes(q);
      const skinHit = champ.skins.some((s) => s.name.toLowerCase().includes(q));
      return nameHit || skinHit;
    }
    return true;
  }

  function renderChampions() {
    const filtered = state.champions.filter(matchesChampion);
    el.championCount.textContent = filtered.length;

    if (filtered.length === 0) {
      el.championList.innerHTML = "";
      el.championEmpty.hidden = false;
      return;
    }
    el.championEmpty.hidden = true;

    el.championList.innerHTML = filtered
      .map((c) => {
        const installedCount = c.skins.filter((s) => installedSet.has(s.id)).length;
        const active = c.id === state.selectedChampId ? "active" : "";
        return `
          <li class="champ-row ${active}" data-id="${c.id}" role="button" tabindex="0">
            <div class="champ-avatar" style="background-image:url('${c.tile}')"></div>
            <span class="champ-name">${c.name}</span>
            <span class="champ-installed">${installedCount > 0 ? installedCount : ""}</span>
          </li>
        `;
      })
      .join("");
  }

  function matchesSkin(skin) {
    if (!state.searchQuery) return true;
    const q = state.searchQuery.toLowerCase();
    return skin.name.toLowerCase().includes(q) || state.selectedChampId.toLowerCase().includes(q);
  }

  function renderSkins() {
    const champ = state.champions.find((c) => c.id === state.selectedChampId);
    if (!champ) {
      el.contentChampName.textContent = "—";
      el.contentChampMeta.textContent = "";
      el.skinGrid.innerHTML = "";
      el.skinEmpty.hidden = false;
      return;
    }
    el.contentChampName.textContent = champ.name;
    el.contentChampMeta.textContent = `${champ.skins.length} skins · ${champ.role}`;

    const skins = champ.skins.filter(matchesSkin);
    if (skins.length === 0) {
      el.skinGrid.innerHTML = "";
      el.skinEmpty.hidden = false;
      return;
    }
    el.skinEmpty.hidden = true;

    el.skinGrid.innerHTML = skins
      .map((s) => {
        const installed = installedSet.has(s.id);
        const action = installed
          ? `<button class="btn-uninstall" data-action="uninstall" data-id="${s.id}">Uninstall</button>`
          : `<button class="btn-install" data-action="install" data-id="${s.id}">Install</button>`;
        const tier = s.tier ? ` · ${s.tier}` : "";
        return `
          <article class="skin-card ${installed ? "installed" : ""}" data-id="${s.id}">
            <div class="skin-art">
              <div class="skin-fallback">
                <span class="skin-fallback-name">${champ.name}</span>
                <span class="skin-fallback-sub">${s.name}</span>
              </div>
              <img src="${s.art}" alt="" loading="lazy"
                   onload="this.classList.add('is-loaded')"
                   onerror="this.style.display='none'" />
            </div>
            <div class="skin-body">
              <h3 class="skin-name">${s.name}</h3>
              <div class="skin-meta">${s.rarity}${tier}</div>
              <div class="skin-actions">${action}</div>
            </div>
          </article>
        `;
      })
      .join("");
  }

  function renderStatus() {
    const totalInstalled = installedSet.size;
    el.installedCount.textContent = `${totalInstalled} installed`;
    el.installedSummary.textContent = `${totalInstalled} skin${totalInstalled === 1 ? "" : "s"} active`;
    el.uninstallAll.disabled = totalInstalled === 0;

    if (state.lolPath) {
      el.lolStatusDot.className = "dot dot-ok";
      el.lolStatusText.textContent = `LoL: ${state.lolPath}`;
      el.lolStatusText.title = state.lolPath;
    } else {
      el.lolStatusDot.className = "dot dot-warn";
      el.lolStatusText.textContent = "League of Legends not detected";
    }
  }

  function renderAll() {
    renderChampions();
    renderSkins();
    renderStatus();
  }

  /* ─────────── Actions ─────────── */

  async function selectChampion(id) {
    state.selectedChampId = id;
    renderChampions();
    renderSkins();
  }

  async function installSkin(skinId) {
    // Look up the skin to find its download URL (.fantome package URL)
    const champ = state.champions.find((c) => c.id === state.selectedChampId);
    const skin = champ?.skins.find((s) => s.id === skinId);
    const downloadUrl = skin?.download || "";
    const card = el.skinGrid.querySelector(`.skin-card[data-id="${CSS.escape(skinId)}"]`);
    const btn = card?.querySelector('button[data-action="install"]');
    if (btn) btn.classList.add("is-busy");
    try {
      const res = await backend("install_skin", { skinId, downloadUrl });
      if (res.ok) {
        installedSet.add(skinId);
        persist();
        toast(`${skinId.replace(/_/g, " ")} installed`, "ok");
      } else {
        toast(`Install failed: ${res.message}`, "err");
      }
    } catch (e) {
      toast(`Install error: ${e.message}`, "err");
    } finally {
      renderAll();
    }
  }

  async function uninstallSkin(skinId) {
    const card = el.skinGrid.querySelector(`.skin-card[data-id="${CSS.escape(skinId)}"]`);
    const btn = card?.querySelector('button[data-action="uninstall"]');
    if (btn) btn.classList.add("is-busy");
    try {
      const res = await backend("uninstall_skin", { skinId });
      if (res.ok) {
        installedSet.delete(skinId);
        persist();
        toast(`${skinId.replace(/_/g, " ")} removed`, "ok");
      } else {
        toast(`Uninstall failed: ${res.message}`, "err");
      }
    } catch (e) {
      toast(`Uninstall error: ${e.message}`, "err");
    } finally {
      renderAll();
    }
  }

  async function restoreAll() {
    if (installedSet.size === 0) return;
    el.uninstallAll.classList.add("is-busy");
    el.uninstallAll.disabled = true;
    try {
      const res = await backend("restore_all");
      if (res.ok) {
        installedSet.clear();
        persist();
        toast("All skins restored", "ok");
      } else {
        toast(`Restore failed: ${res.message}`, "err");
      }
    } finally {
      renderAll();
    }
  }

  /* ─────────── Add Skin Modal ─────────── */

  const modal = {
    mode: "file", // 'file' | 'url'
    file: null,   // { name, size, path }
    url: "",       // string

    open() {
      el.addModal.hidden = false;
      this.reset();
      setTimeout(() => el.dropzone.focus(), 30);
    },
    close() {
      el.addModal.hidden = true;
      this.reset();
    },
    reset() {
      this.mode = "file";
      this.file = null;
      this.url = "";
      el.fileInput.value = "";
      el.urlInput.value = "";
      el.fileInfo.hidden = true;
      el.addConfirm.disabled = true;
      $$(".modal-tab").forEach((t) => t.classList.toggle("active", t.dataset.tab === "file"));
      $$(".modal-pane").forEach((p) => (p.hidden = p.dataset.pane !== "file"));
    },
    setMode(mode) {
      this.mode = mode;
      $$(".modal-tab").forEach((t) => t.classList.toggle("active", t.dataset.tab === mode));
      $$(".modal-pane").forEach((p) => (p.hidden = p.dataset.pane !== mode));
      this.updateConfirm();
    },
    setFile(file) {
      this.file = { name: file.name, size: file.size };
      el.fileName.textContent = file.name;
      el.fileMeta.textContent = fmtBytes(file.size);
      el.fileInfo.hidden = false;
      this.updateConfirm();
    },
    setUrl(url) {
      this.url = url;
      this.updateConfirm();
    },
    updateConfirm() {
      const ok = this.mode === "file"
        ? Boolean(this.file)
        : Boolean(this.url) && /^https?:\/\//i.test(this.url);
      el.addConfirm.disabled = !ok;
    },
    async confirm() {
      el.addConfirm.classList.add("is-busy");
      el.addConfirm.disabled = true;
      try {
        let res;
        if (this.mode === "file") {
          res = await backend("add_local_skin", { filePath: this.file.name });
        } else {
          res = await backend("add_url_skin", { url: this.url });
        }
        if (res.ok) {
          toast(res.message, "ok");
          this.close();
        } else {
          toast(res.message, "err");
        }
      } catch (e) {
        toast(`Error: ${e.message}`, "err");
      } finally {
        el.addConfirm.classList.remove("is-busy");
        this.updateConfirm();
      }
    },
  };

  /* ─────────── Event wiring ─────────── */

  function wire() {
    // Champion list click + keyboard
    el.championList.addEventListener("click", (e) => {
      const row = e.target.closest(".champ-row");
      if (row) selectChampion(row.dataset.id);
    });
    el.championList.addEventListener("keydown", (e) => {
      const row = e.target.closest(".champ-row");
      if (row && (e.key === "Enter" || e.key === " ")) {
        e.preventDefault();
        selectChampion(row.dataset.id);
      }
    });

    // Skin grid click (install / uninstall)
    el.skinGrid.addEventListener("click", (e) => {
      const btn = e.target.closest("button[data-action]");
      if (!btn) return;
      const id = btn.dataset.id;
      if (btn.dataset.action === "install") installSkin(id);
      else if (btn.dataset.action === "uninstall") uninstallSkin(id);
    });

    // Search
    const onSearch = debounce((v) => {
      state.searchQuery = v.trim();
      el.searchClear.hidden = state.searchQuery.length === 0;
      renderAll();
    }, 100);
    el.searchInput.addEventListener("input", (e) => onSearch(e.target.value));
    el.searchClear.addEventListener("click", () => {
      el.searchInput.value = "";
      onSearch("");
      el.searchInput.focus();
    });

    // "/" focuses search
    document.addEventListener("keydown", (e) => {
      if (e.key === "/" && document.activeElement !== el.searchInput) {
        e.preventDefault();
        el.searchInput.focus();
      }
      if (e.key === "Escape") {
        if (!el.addModal.hidden) modal.close();
        else if (document.activeElement === el.searchInput) {
          el.searchInput.value = "";
          onSearch("");
        }
      }
    });

    // Role filter
    el.roleFilter.addEventListener("change", (e) => {
      state.roleFilter = e.target.value;
      renderChampions();
    });

    // Source toggle
    el.sourceLocal.addEventListener("click", () => {
      state.source = "local";
      el.sourceLocal.classList.add("active");
      el.sourceRepo.classList.remove("active");
    });
    el.sourceRepo.addEventListener("click", () => {
      state.source = "repo";
      el.sourceRepo.classList.add("active");
      el.sourceLocal.classList.remove("active");
      modal.open();
    });

    // Add skin button
    el.addSkinBtn.addEventListener("click", () => modal.open());
    el.settingsBtn.addEventListener("click", () => toast("Settings panel — not in prototype"));

    // Uninstall all
    el.uninstallAll.addEventListener("click", restoreAll);

    // Modal: close (event delegation handles backdrop click + close button click,
    // even when the click lands on an SVG child inside the close button).
    el.addModal.addEventListener("click", (e) => {
      if (e.target.closest("[data-close]")) modal.close();
    });
    $$(".modal-tab").forEach((tab) =>
      tab.addEventListener("click", () => modal.setMode(tab.dataset.tab))
    );

    // Modal: file dropzone
    el.dropzone.addEventListener("click", () => el.fileInput.click());
    el.dropzone.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        el.fileInput.click();
      }
    });
    el.dropzone.addEventListener("dragover", (e) => {
      e.preventDefault();
      el.dropzone.classList.add("dragover");
    });
    el.dropzone.addEventListener("dragleave", () => el.dropzone.classList.remove("dragover"));
    el.dropzone.addEventListener("drop", (e) => {
      e.preventDefault();
      el.dropzone.classList.remove("dragover");
      const file = e.dataTransfer.files?.[0];
      if (file) modal.setFile(file);
    });
    el.fileInput.addEventListener("change", (e) => {
      const file = e.target.files?.[0];
      if (file) modal.setFile(file);
    });
    el.fileClear.addEventListener("click", () => {
      modal.file = null;
      el.fileInput.value = "";
      el.fileInfo.hidden = true;
      modal.updateConfirm();
    });

    // Modal: URL input
    el.urlInput.addEventListener("input", (e) => modal.setUrl(e.target.value));
    $$(".chip[data-url]").forEach((chip) =>
      chip.addEventListener("click", () => {
        el.urlInput.value = chip.dataset.url;
        modal.setUrl(chip.dataset.url);
      })
    );

    // Modal: confirm
    el.addConfirm.addEventListener("click", () => modal.confirm());
  }

  /* ─────────── Bootstrap ─────────── */

  async function bootstrap() {
    el.appVersion.textContent = `v${window.VEIL_DATA.version}`;
    loadPersisted();
    wire();

    // Detect LoL (mock path detection)
    try {
      const path = await backend("get_lol_path");
      state.lolPath = path;
    } catch (_) {
      state.lolPath = null;
    }
    renderStatus();

    // Load champions
    try {
      state.champions = await backend("get_champions");
      if (state.champions.length > 0) {
        state.selectedChampId = state.champions[0].id;
      }
    } catch (e) {
      toast(`Failed to load champions: ${e.message}`, "err");
    }

    renderAll();

    // Notify the user (Tauri-only path is silent)
    if (!isTauri) {
      console.info("[Veil] Running in browser-preview mode. Backend is mocked.");
    }
  }

  document.addEventListener("DOMContentLoaded", bootstrap);
})();
