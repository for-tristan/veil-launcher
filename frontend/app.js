/* ─────────────────────────────────────────────────────────────
   Veil launcher — app.js
   Vanilla JS controller. Detects Tauri at runtime; if running
   outside Tauri (browser preview), falls back to a JS mock backend
   so the UI is fully interactive without Rust.
   ───────────────────────────────────────────────────────────── */

(() => {
  "use strict";

  /* ─────────── Tauri / mock backend bridge ─────────── */

  const isTauri = typeof window !== "undefined" && (
    Boolean(window.__TAURI_INTERNALS__) || Boolean(window.__TAURI__)
  );
  const tauriApi = isTauri ? window.__TAURI__ : null;
  const invoke = tauriApi?.core?.invoke || null;
  const tauriListen = tauriApi?.event?.listen || null;

  if (isTauri && !invoke) {
    console.error(
      "[Veil] Running inside Tauri but window.__TAURI__.core.invoke is missing. " +
      "Did you set `app.withGlobalTauri: true` in tauri.conf.json? " +
      "Falling back to mock backend — apply/uninstall will NOT actually do anything."
    );
  }

  const wait = (ms) => new Promise((r) => setTimeout(r, ms));

  // In-memory applied skins set (champion_id → skin_id), kept in sync
  // with the Rust backend's persisted state file.
  const appliedSkins = new Map(); // champion_id → skin_id

  function persist() {
    if (!isTauri) {
      const arr = [...appliedSkins.entries()].map(([c, s]) => `${c}=${s}`);
      localStorage.setItem("veil.applied", arr.join("|"));
    }
  }

  function loadPersistedBrowser() {
    if (isTauri) return;
    try {
      const saved = localStorage.getItem("veil.applied") || "";
      saved.split("|").filter(Boolean).forEach((pair) => {
        const [c, s] = pair.split("=");
        if (c && s) appliedSkins.set(c, s);
      });
    } catch (_) {}
  }

  // Mock backend for browser preview. Mirrors the Rust command names so
  // app logic is identical between preview and built app.
  const mockBackend = {
    async get_lol_path() {
      await wait(600);
      return window.VEIL_DATA.lolPath;
    },
    async get_champions() {
      await wait(120);
      return window.VEIL_DATA.champions;
    },
    async apply_skin(skinId) {
      await wait(1200 + Math.random() * 600);
      const champId = skinId.split("_")[0];
      // Simulate one-skin-per-champ rule
      if (appliedSkins.get(champId) && appliedSkins.get(champId) !== skinId) {
        await wait(500);
      }
      appliedSkins.set(champId, skinId);
      persist();
      return { ok: true, message: "Skin applied (mock — dry-run mode in browser preview)." };
    },
    async uninstall_skin(skinId) {
      await wait(600);
      const champId = skinId.split("_")[0];
      appliedSkins.delete(champId);
      persist();
      return { ok: true, message: "Skin removed (mock)." };
    },
    async restore_all() {
      await wait(1200);
      appliedSkins.clear();
      persist();
      return { ok: true, message: "All skins restored (mock)." };
    },
    async add_local_skin(filePath) {
      await wait(900);
      return { ok: true, message: `Skin file '${filePath}' queued.` };
    },
    async add_url_skin(url) {
      await wait(1100);
      if (url.includes("404")) return { ok: false, message: "Download failed (404)." };
      return { ok: true, message: "Skin URL accepted." };
    },
    async check_league_running() {
      return false;
    },
    async get_applied_skins() {
      const obj = {};
      appliedSkins.forEach((v, k) => { obj[k] = v; });
      return obj;
    },
    async get_settings() {
      return { dryRun: true, lolPathOverride: null };
    },
    async set_settings() {
      return;
    },
  };

  async function backend(name, ...args) {
    if (isTauri && invoke) {
      return invoke(name, args[0] || {});
    }
    if (mockBackend[name]) return mockBackend[name](...args);
    throw new Error(`Unknown backend command: ${name}`);
  }

  // Wire up the apply-progress Tauri event listener (only meaningful in Tauri).
  if (isTauri && tauriListen) {
    tauriListen("apply-progress", (event) => {
      const payload = event.payload || {};
      handleApplyProgress(payload);
    });
  }

  /* ─────────── App state ─────────── */

  const state = {
    champions: [],
    selectedChampId: null,
    searchQuery: "",
    roleFilter: "all",
    lolPath: null,
    leagueRunning: false,
    settings: { dryRun: true, lolPathOverride: null },
    // Currently-in-flight apply operations: skinId → { phase, percent, message }
    activeProgress: new Map(),
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
    appliedCount: $("#applied-count"),
    appliedSummary: $("#applied-summary"),
    lolStatusDot: $("#lol-status-dot"),
    lolStatusText: $("#lol-status-text"),
    leagueRunningWarning: $("#league-running-warning"),
    appVersion: $("#app-version"),
    addSkinBtn: $("#add-skin-btn"),
    settingsBtn: $("#settings-btn"),
    addModal: $("#add-modal"),
    settingsModal: $("#settings-modal"),
    settingsLolPath: $("#settings-lol-path"),
    settingsToolsDir: $("#settings-tools-dir"),
    settingsDryRunToggle: $("#settings-dry-run-toggle"),
    settingsDryRunLabel: $("#settings-dry-run-label"),
    settingsRestoreBtn: $("#settings-restore-btn"),
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
    toastTimer = setTimeout(() => (el.toast.hidden = true), 3200);
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
        const hasApplied = appliedSkins.has(c.id);
        const active = c.id === state.selectedChampId ? "active" : "";
        return `
          <li class="champ-row ${active}" data-id="${c.id}" role="button" tabindex="0">
            <div class="champ-avatar" style="background-image:url('${c.tile}')"></div>
            <span class="champ-name">${c.name}</span>
            ${hasApplied ? '<span class="champ-installed">1</span>' : ""}
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
        const isApplied = appliedSkins.get(champ.id) === s.id;
        // If another skin of this champion is applied, show "Replace" instead of "Apply"
        const otherApplied = appliedSkins.has(champ.id) && !isApplied;
        const actionLabel = isApplied ? "Remove" : (otherApplied ? "Replace" : "Apply");
        const actionClass = isApplied ? "btn-uninstall" : "btn-install";
        const actionName = isApplied ? "uninstall" : "apply";
        const tier = s.tier ? ` · ${s.tier}` : "";
        return `
          <article class="skin-card ${isApplied ? "installed" : ""}" data-id="${s.id}">
            <div class="skin-art">
              <div class="skin-fallback">
                <span class="skin-fallback-name">${champ.name}</span>
                <span class="skin-fallback-sub">${s.name}</span>
              </div>
              <img src="${s.art}" alt="" loading="lazy"
                   onload="this.classList.add('is-loaded')"
                   onerror="this.style.display='none'" />
              <div class="skin-progress" hidden>
                <div class="progress-info">
                  <span class="progress-phase">Starting…</span>
                  <span class="progress-percent">0%</span>
                </div>
                <div class="progress-bar"><div class="progress-fill"></div></div>
                <div class="progress-message"></div>
              </div>
            </div>
            <div class="skin-body">
              <h3 class="skin-name">${s.name}</h3>
              <div class="skin-meta">${s.rarity}${tier}</div>
              <div class="skin-actions">
                <button class="${actionClass}" data-action="${actionName}" data-id="${s.id}">${actionLabel}</button>
              </div>
            </div>
          </article>
        `;
      })
      .join("");
  }

  function renderStatus() {
    const totalApplied = appliedSkins.size;
    el.appliedCount.textContent = `${totalApplied} applied`;
    el.appliedSummary.textContent = `${totalApplied} skin${totalApplied === 1 ? "" : "s"} active`;

    if (state.lolPath) {
      el.lolStatusDot.className = "dot dot-ok";
      el.lolStatusText.textContent = `LoL: ${state.lolPath}`;
      el.lolStatusText.title = state.lolPath;
    } else {
      el.lolStatusDot.className = "dot dot-warn";
      el.lolStatusText.textContent = "League of Legends not detected";
    }

    el.leagueRunningWarning.hidden = !state.leagueRunning;
  }

  function renderAll() {
    renderChampions();
    renderSkins();
    renderStatus();
  }

  /* ─────────── Apply progress event handler ─────────── */

  function handleApplyProgress(payload) {
    const { skinId, phase, percent, message, total, current } = payload;
    if (!skinId) return;

    const card = el.skinGrid.querySelector(`.skin-card[data-id="${CSS.escape(skinId)}"]`);
    if (!card) return;

    const progress = card.querySelector(".skin-progress");
    const phaseEl = progress?.querySelector(".progress-phase");
    const percentEl = progress?.querySelector(".progress-percent");
    const fillEl = progress?.querySelector(".progress-fill");
    const messageEl = progress?.querySelector(".progress-message");

    if (!progress || !phaseEl || !percentEl || !fillEl) return;

    progress.hidden = false;

    const phaseLabels = {
      starting: "Starting…",
      cached: "Using cache",
      downloading: "Downloading",
      downloaded: "Downloaded",
      extracting: "Extracting",
      replacing: "Replacing previous skin",
      "finding-tools": "Finding LTK tools",
      "creating-overlay": "Creating overlay",
      "starting-patcher": "Starting patcher",
      injecting: current != null && total
        ? `Injecting (${current}/${total})`
        : "Injecting",
      "injecting-pending": "Queued for injection",
      injected: "Injected",
      done: "Done",
      error: "Error",
    };
    phaseEl.textContent = phaseLabels[phase] || phase;

    if (percent != null) {
      percentEl.textContent = `${Math.round(percent)}%`;
      fillEl.style.width = `${Math.min(100, Math.round(percent))}%`;
    } else {
      percentEl.textContent = "";
      fillEl.style.width = "0%";
    }

    if (message) {
      messageEl.textContent = message;
      messageEl.className = "progress-message" + (phase === "error" ? " err" : phase === "done" ? " ok" : "");
    }

    if (phase === "done") {
      setTimeout(() => { progress.hidden = true; }, 800);
    } else if (phase === "error") {
      // Leave the error visible until the user starts a new operation.
    }
  }

  /* ─────────── Actions ─────────── */

  async function selectChampion(id) {
    state.selectedChampId = id;
    renderChampions();
    renderSkins();
  }

  async function applySkin(skinId) {
    const champ = state.champions.find((c) => c.id === state.selectedChampId);
    const skin = champ?.skins.find((s) => s.id === skinId);
    if (!skin) return;
    const downloadUrl = skin.download || "";

    const card = el.skinGrid.querySelector(`.skin-card[data-id="${CSS.escape(skinId)}"]`);
    const btn = card?.querySelector('button[data-action="apply"]');
    if (btn) btn.classList.add("is-busy");

    // Show the progress overlay immediately with a "Starting" phase
    handleApplyProgress({ skinId, phase: "starting", percent: 0 });

    try {
      const res = await backend("apply_skin", { skinId, downloadUrl });
      if (res.ok) {
        // For one-skin-per-champ: clear any other skin of this champion
        const champId = skinId.split("_")[0];
        appliedSkins.set(champId, skinId);
        persist();
        toast(res.message || `${skin.name} applied`, "ok");
      } else {
        toast(`Apply failed: ${res.message}`, "err");
      }
    } catch (e) {
      handleApplyProgress({ skinId, phase: "error", message: e.message });
      toast(`Apply error: ${e.message}`, "err");
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
        const champId = skinId.split("_")[0];
        appliedSkins.delete(champId);
        persist();
        toast(res.message || "Skin removed", "ok");
      } else {
        toast(`Remove failed: ${res.message}`, "err");
      }
    } catch (e) {
      toast(`Remove error: ${e.message}`, "err");
    } finally {
      renderAll();
    }
  }

  async function restoreAll() {
    if (appliedSkins.size === 0) {
      toast("No skins to restore.");
      return;
    }
    el.settingsRestoreBtn.classList.add("is-busy");
    el.settingsRestoreBtn.disabled = true;
    try {
      const res = await backend("restore_all");
      if (res.ok) {
        appliedSkins.clear();
        persist();
        toast(res.message || "All skins restored", "ok");
      } else {
        toast(`Restore failed: ${res.message}`, "err");
      }
    } catch (e) {
      toast(`Restore error: ${e.message}`, "err");
    } finally {
      renderAll();
      el.settingsRestoreBtn.classList.remove("is-busy");
      el.settingsRestoreBtn.disabled = false;
    }
  }

  /* ─────────── Add Skin modal ─────────── */

  const addModal = {
    mode: "file",
    file: null,
    url: "",

    open() { el.addModal.hidden = false; this.reset(); setTimeout(() => el.dropzone.focus(), 30); },
    close() { el.addModal.hidden = true; this.reset(); },
    reset() {
      this.mode = "file"; this.file = null; this.url = "";
      el.fileInput.value = ""; el.urlInput.value = "";
      el.fileInfo.hidden = true; el.addConfirm.disabled = true;
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
    setUrl(url) { this.url = url; this.updateConfirm(); },
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
        if (res.ok) { toast(res.message, "ok"); this.close(); }
        else { toast(res.message, "err"); }
      } catch (e) {
        toast(`Error: ${e.message}`, "err");
      } finally {
        el.addConfirm.classList.remove("is-busy");
        this.updateConfirm();
      }
    },
  };

  /* ─────────── Settings modal ─────────── */

  const settingsModal = {
    open() {
      el.settingsModal.hidden = false;
      this.refresh();
    },
    close() {
      el.settingsModal.hidden = true;
    },
    async refresh() {
      // LoL path
      el.settingsLolPath.textContent = state.lolPath || "Not detected";
      // Tools directory
      el.settingsToolsDir.value = state.settings.toolsDir || "";
      // Dry Run toggle
      el.settingsDryRunToggle.checked = state.settings.dryRun;
      this.updateDryRunLabel();
    },
    updateDryRunLabel() {
      const on = el.settingsDryRunToggle.checked;
      el.settingsDryRunLabel.textContent = on
        ? "Dry Run is ON — native WAD modification mode (experimental, may not work)"
        : "Real injection is ON — uses ltk_patcher subprocess (recommended)";
    },
    async toggleDryRun() {
      const dryRun = el.settingsDryRunToggle.checked;
      this.updateDryRunLabel();
      try {
        await backend("set_settings", { dryRun });
        state.settings.dryRun = dryRun;
        toast(dryRun
          ? "Dry Run enabled — Apply will skip the patcher and try native WAD modification (may fail)."
          : "Real injection enabled — Apply uses ltk_patcher (recommended).",
          dryRun ? "default" : "ok");
      } catch (e) {
        toast(`Failed to save setting: ${e.message}`, "err");
        el.settingsDryRunToggle.checked = !dryRun;
        this.updateDryRunLabel();
      }
    },
    async saveToolsDir() {
      const toolsDir = el.settingsToolsDir.value.trim() || null;
      try {
        await backend("set_settings", { toolsDir });
        state.settings.toolsDir = toolsDir;
        toast(toolsDir
          ? `Tools directory set to: ${toolsDir}`
          : "Tools directory cleared — Veil will auto-detect.");
      } catch (e) {
        toast(`Failed to save tools directory: ${e.message}`, "err");
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

    // Skin grid click (apply / uninstall)
    el.skinGrid.addEventListener("click", (e) => {
      const btn = e.target.closest("button[data-action]");
      if (!btn) return;
      const id = btn.dataset.id;
      if (btn.dataset.action === "apply") applySkin(id);
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

    // "/" focuses search, Esc closes modals
    document.addEventListener("keydown", (e) => {
      if (e.key === "/" && document.activeElement !== el.searchInput) {
        e.preventDefault();
        el.searchInput.focus();
      }
      if (e.key === "Escape") {
        if (!el.settingsModal.hidden) settingsModal.close();
        else if (!el.addModal.hidden) addModal.close();
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

    // Add skin button
    el.addSkinBtn.addEventListener("click", () => addModal.open());
    // Settings button
    el.settingsBtn.addEventListener("click", () => settingsModal.open());

    // Add modal: close
    el.addModal.addEventListener("click", (e) => {
      if (e.target.closest("[data-close]")) addModal.close();
    });
    $$(".modal-tab").forEach((tab) =>
      tab.addEventListener("click", () => addModal.setMode(tab.dataset.tab))
    );
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
      if (file) addModal.setFile(file);
    });
    el.fileInput.addEventListener("change", (e) => {
      const file = e.target.files?.[0];
      if (file) addModal.setFile(file);
    });
    el.fileClear.addEventListener("click", () => {
      addModal.file = null;
      el.fileInput.value = "";
      el.fileInfo.hidden = true;
      addModal.updateConfirm();
    });
    el.urlInput.addEventListener("input", (e) => addModal.setUrl(e.target.value));
    $$(".chip[data-url]").forEach((chip) =>
      chip.addEventListener("click", () => {
        el.urlInput.value = chip.dataset.url;
        addModal.setUrl(chip.dataset.url);
      })
    );
    el.addConfirm.addEventListener("click", () => addModal.confirm());

    // Settings modal: close (event delegation)
    el.settingsModal.addEventListener("click", (e) => {
      if (e.target.closest("[data-close]")) settingsModal.close();
    });
    el.settingsDryRunToggle.addEventListener("change", () => settingsModal.toggleDryRun());
    el.settingsToolsDir.addEventListener("change", () => settingsModal.saveToolsDir());
    el.settingsToolsDir.addEventListener("blur", () => settingsModal.saveToolsDir());
    el.settingsRestoreBtn.addEventListener("click", () => {
      settingsModal.close();
      restoreAll();
    });
  }

  /* ─────────── Bootstrap ─────────── */

  async function bootstrap() {
    el.appVersion.textContent = `v${window.VEIL_DATA.version}`;
    loadPersistedBrowser();
    wire();

    // Detect LoL
    try {
      const path = await backend("get_lol_path");
      state.lolPath = path;
    } catch (_) {
      state.lolPath = null;
    }

    // Load settings
    try {
      const s = await backend("get_settings");
      state.settings = s || { dryRun: true, lolPathOverride: null };
    } catch (_) {
      state.settings = { dryRun: true, lolPathOverride: null };
    }

    // Load champions
    try {
      state.champions = await backend("get_champions");
      if (state.champions.length > 0) {
        state.selectedChampId = state.champions[0].id;
      }
    } catch (e) {
      toast(`Failed to load champions: ${e.message}`, "err");
    }

    // Sync applied skins state with backend
    try {
      const applied = await backend("get_applied_skins");
      if (applied && typeof applied === "object") {
        appliedSkins.clear();
        Object.entries(applied).forEach(([champId, skinId]) => {
          appliedSkins.set(champId, skinId);
        });
      }
    } catch (_) {}

    // Check if League is running (and refresh every 30s)
    async function refreshLeagueRunning() {
      try {
        state.leagueRunning = await backend("check_league_running");
      } catch (_) {
        state.leagueRunning = false;
      }
      renderStatus();
    }
    refreshLeagueRunning();
    setInterval(refreshLeagueRunning, 30000);

    renderAll();

    if (!isTauri) {
      console.info("[Veil] Running in browser-preview mode. Backend is mocked.");
    }
  }

  document.addEventListener("DOMContentLoaded", bootstrap);
})();
