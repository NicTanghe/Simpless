const appState = {
  overview: null,
};

document.addEventListener("DOMContentLoaded", () => {
  const elements = {
    originInput: document.getElementById("activator-origin"),
    connectionForm: document.getElementById("connection-form"),
    overviewStatus: document.getElementById("overview-status"),
    configPath: document.getElementById("config-path"),
    uploadDir: document.getElementById("upload-dir"),
    serviceCount: document.getElementById("service-count"),
    runningCount: document.getElementById("running-count"),
    binaryCount: document.getElementById("binary-count"),
    servicesList: document.getElementById("services-list"),
    binariesList: document.getElementById("binaries-list"),
    uploadForm: document.getElementById("upload-form"),
    uploadInput: document.getElementById("binary-file"),
    uploadStatus: document.getElementById("upload-status"),
    uploadSqlPreview: document.getElementById("upload-sql-preview"),
    sqlForm: document.getElementById("sql-form"),
    sqlEditor: document.getElementById("sql-editor"),
    sqlStatus: document.getElementById("sql-status"),
    sqlResult: document.getElementById("sql-result"),
  };

  if (!elements.originInput) {
    return;
  }

  elements.originInput.value = loadActivatorOrigin();
  bindParallax();

  elements.connectionForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    saveActivatorOrigin(elements.originInput.value);
    await refreshOverview(elements, "Refreshing activator overview...");
  });

  elements.uploadForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    await uploadBinaries(elements);
  });

  elements.sqlForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    await runSql(elements);
  });

  elements.servicesList.addEventListener("click", (event) => {
    const button = event.target.closest("[data-fill-service-sql]");
    if (!button || !appState.overview) {
      return;
    }

    const index = Number(button.getAttribute("data-fill-service-sql"));
    const service = appState.overview.services[index];
    if (!service) {
      return;
    }

    elements.sqlEditor.value = buildServiceUpdateSql(service);
    elements.sqlEditor.focus();
    setStatus(elements.sqlStatus, `Loaded UPDATE statement for /${service.route_prefix}.`, "good");
  });

  elements.binariesList.addEventListener("click", (event) => {
    const button = event.target.closest("[data-fill-binary-sql]");
    if (!button || !appState.overview) {
      return;
    }

    const index = Number(button.getAttribute("data-fill-binary-sql"));
    const binary = appState.overview.binaries[index];
    if (!binary) {
      return;
    }

    const template = buildBinaryInsertSql(binary);
    elements.sqlEditor.value = template;
    elements.uploadSqlPreview.textContent = template;
    elements.sqlEditor.focus();
    setStatus(elements.sqlStatus, `Loaded INSERT statement for ${binary.name}.`, "good");
  });

  refreshOverview(elements, "Connecting to activator...");
});

function bindParallax() {
  const root = document.documentElement;

  const update = () => {
    root.style.setProperty("--scroll-y", `${window.scrollY}px`);
    document.querySelectorAll("[data-parallax-speed]").forEach((node) => {
      node.style.setProperty("--speed", node.getAttribute("data-parallax-speed") || "0");
    });
  };

  update();
  window.addEventListener("scroll", update, { passive: true });
  window.addEventListener("resize", update, { passive: true });
}

async function refreshOverview(elements, loadingMessage) {
  setStatus(elements.overviewStatus, loadingMessage || "Refreshing...", "warn");

  try {
    const overview = await fetchJson(buildAdminUrl(elements.originInput.value, "/overview"));
    appState.overview = overview;

    const runningCount = overview.services.filter((service) => service.running).length;
    elements.serviceCount.textContent = String(overview.services.length);
    elements.runningCount.textContent = String(runningCount);
    elements.binaryCount.textContent = String(overview.binaries.length);
    elements.configPath.textContent = overview.config_path;
    elements.uploadDir.textContent = overview.upload_dir;
    setStatus(
      elements.overviewStatus,
      `Connected. ${overview.services.length} services, ${runningCount} running.`,
      "good",
    );

    renderServices(elements.servicesList, overview.services);
    renderBinaries(elements.binariesList, overview.binaries);
  } catch (error) {
    setStatus(elements.overviewStatus, `Activator request failed: ${error.message}`, "bad");
    elements.configPath.textContent = "unreachable";
    elements.uploadDir.textContent = "unreachable";
    renderEmpty(
      elements.servicesList,
      "Could not load services from the activator. Check the origin and that the gateway is running.",
    );
    renderEmpty(
      elements.binariesList,
      "Could not load uploaded binaries from the activator.",
    );
  }
}

function renderServices(container, services) {
  if (!services.length) {
    renderEmpty(container, "The activator registry is empty.");
    return;
  }

  container.innerHTML = services
    .map((service, index) => {
      const status = service.last_startup_error
        ? {
            label: "startup issue",
            className: "status-pill-error",
          }
        : service.running
          ? {
              label: "running",
              className: "status-pill-running",
            }
          : {
              label: "sleeping",
              className: "status-pill-sleeping",
            };

      return `
        <article class="service-card">
          <div class="card-topline">
            <div>
              <p class="card-title">/${escapeHtml(service.route_prefix)}</p>
              <p class="service-copy">${escapeHtml(service.command)} ${escapeHtml(service.args.join(" "))}</p>
            </div>
            <span class="status-pill ${status.className}">${status.label}</span>
          </div>
          <div class="meta-grid">
            <div>
              <span>backend</span>
              <strong>${escapeHtml(service.backend_base_url)}</strong>
            </div>
            <div>
              <span>health</span>
              <code>${escapeHtml(service.health_path)}</code>
            </div>
            <div>
              <span>working directory</span>
              <code>${escapeHtml(service.working_directory || "inherit activator cwd")}</code>
            </div>
            <div>
              <span>timeouts</span>
              <strong>${service.startup_timeout_ms} ms startup / ${service.idle_timeout_secs}s idle</strong>
            </div>
            <div>
              <span>environment keys</span>
              <strong>${Object.keys(service.environment || {}).length}</strong>
            </div>
            <div>
              <span>last used</span>
              <strong>${formatDuration(service.last_used_ms_ago)}</strong>
            </div>
          </div>
          ${
            service.last_startup_error
              ? `<p class="service-copy">Last startup error: ${escapeHtml(service.last_startup_error)}</p>`
              : ""
          }
          <div class="card-actions">
            <button class="secondary-button" type="button" data-fill-service-sql="${index}">
              Edit via SQL
            </button>
          </div>
        </article>
      `;
    })
    .join("");
}

function renderBinaries(container, binaries) {
  if (!binaries.length) {
    renderEmpty(container, "No binaries have been uploaded to the activator yet.");
    return;
  }

  container.innerHTML = binaries
    .map(
      (binary, index) => `
        <article class="binary-card">
          <div class="card-topline">
            <div>
              <p class="card-title">${escapeHtml(binary.name)}</p>
              <p class="binary-copy">${escapeHtml(binary.stored_path)}</p>
            </div>
            <span class="status-pill">${formatBytes(binary.size_bytes)}</span>
          </div>
          <div class="meta-grid">
            <div>
              <span>stored path</span>
              <code>${escapeHtml(binary.stored_path)}</code>
            </div>
            <div>
              <span>modified</span>
              <strong>${binary.modified_unix_ms ? formatTimestamp(binary.modified_unix_ms) : "unknown"}</strong>
            </div>
          </div>
          <div class="card-actions">
            <button class="secondary-button" type="button" data-fill-binary-sql="${index}">
              Build INSERT SQL
            </button>
          </div>
        </article>
      `,
    )
    .join("");
}

async function uploadBinaries(elements) {
  const files = Array.from(elements.uploadInput.files || []);
  if (!files.length) {
    setStatus(elements.uploadStatus, "Choose at least one file before uploading.", "bad");
    return;
  }

  const formData = new FormData();
  files.forEach((file) => {
    formData.append("binary", file, file.name);
  });

  setStatus(elements.uploadStatus, `Uploading ${files.length} file(s)...`, "warn");

  try {
    const result = await fetchJson(buildAdminUrl(elements.originInput.value, "/upload"), {
      method: "POST",
      body: formData,
    });

    const templates = result.uploaded.map((item) => item.sql_template);
    elements.uploadSqlPreview.textContent = templates.join("\n\n");
    if (templates.length) {
      elements.sqlEditor.value = templates[0];
    }

    setStatus(
      elements.uploadStatus,
      `Uploaded ${result.uploaded.length} file(s) to the activator.`,
      "good",
    );
    elements.uploadInput.value = "";
    await refreshOverview(elements, "Refreshing after upload...");
  } catch (error) {
    setStatus(elements.uploadStatus, `Upload failed: ${error.message}`, "bad");
  }
}

async function runSql(elements) {
  const sql = elements.sqlEditor.value.trim();
  if (!sql) {
    setStatus(elements.sqlStatus, "SQL editor is empty.", "bad");
    return;
  }

  setStatus(elements.sqlStatus, "Running SQL against the activator...", "warn");

  try {
    const result = await fetchJson(buildAdminUrl(elements.originInput.value, "/sql"), {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ sql }),
    });

    renderSqlResult(elements.sqlResult, result);
    if (result.kind === "query") {
      setStatus(elements.sqlStatus, `Query returned ${result.row_count} row(s).`, "good");
    } else {
      setStatus(
        elements.sqlStatus,
        `Statement changed ${result.rows_affected} row(s) and reloaded the activator registry.`,
        "good",
      );
      await refreshOverview(elements, "Refreshing after SQL change...");
    }
  } catch (error) {
    setStatus(elements.sqlStatus, `SQL failed: ${error.message}`, "bad");
    elements.sqlResult.innerHTML = "";
  }
}

function renderSqlResult(container, result) {
  if (result.kind !== "query") {
    container.innerHTML = `
      <div class="status-bar status-tone-good">
        Statement applied. Rows affected: ${result.rows_affected}.
      </div>
    `;
    return;
  }

  if (!result.rows.length) {
    container.innerHTML = `
      <div class="status-bar status-tone-warn">
        Query succeeded but returned no rows.
      </div>
    `;
    return;
  }

  const head = result.columns
    .map((column) => `<th>${escapeHtml(column)}</th>`)
    .join("");
  const body = result.rows
    .map((row) => {
      const cells = row
        .map((cell) => `<td>${escapeHtml(formatCell(cell))}</td>`)
        .join("");
      return `<tr>${cells}</tr>`;
    })
    .join("");

  container.innerHTML = `
    <div class="table-wrap">
      <table>
        <thead>
          <tr>${head}</tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderEmpty(container, message) {
  container.innerHTML = `<div class="empty-card">${escapeHtml(message)}</div>`;
}

function buildServiceUpdateSql(service) {
  return `UPDATE services
SET command = '${escapeSql(service.command)}',
    args_json = '${escapeSql(JSON.stringify(service.args || []))}',
    backend_port = ${service.backend_port},
    strip_prefix = ${service.strip_prefix ? 1 : 0},
    environment_json = '${escapeSql(JSON.stringify(service.environment || {}))}',
    working_directory = ${service.working_directory ? `'${escapeSql(service.working_directory)}'` : "NULL"},
    startup_timeout_ms = ${service.startup_timeout_ms},
    idle_timeout_secs = ${service.idle_timeout_secs},
    health_path = '${escapeSql(service.health_path)}'
WHERE route_prefix = '${escapeSql(service.route_prefix)}';`;
}

function buildBinaryInsertSql(binary) {
  const routePrefix = slugFromName(binary.name);
  return `-- edit route_prefix and backend_port before running
INSERT INTO services (
    route_prefix,
    command,
    args_json,
    backend_port,
    strip_prefix,
    environment_json,
    working_directory,
    startup_timeout_ms,
    idle_timeout_secs,
    health_path
) VALUES (
    '${escapeSql(routePrefix)}',
    '${escapeSql(binary.stored_path)}',
    '[]',
    9100,
    1,
    '{}',
    NULL,
    15000,
    120,
    '/health'
);`;
}

function slugFromName(name) {
  const stem = name.replace(/\.[^.]+$/, "");
  const slug = stem
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return slug || "service";
}

async function fetchJson(url, options) {
  const response = await fetch(url, options);
  const text = await response.text();
  let payload = {};

  if (text) {
    try {
      payload = JSON.parse(text);
    } catch (_error) {
      payload = { error: text };
    }
  }

  if (!response.ok) {
    throw new Error(payload.error || `${response.status} ${response.statusText}`);
  }

  return payload;
}

function buildAdminUrl(originInput, path) {
  const origin = normalizeOrigin(originInput || loadActivatorOrigin());
  return `${origin}/_admin/api${path}`;
}

function normalizeOrigin(value) {
  return String(value || "").trim().replace(/\/+$/, "");
}

function loadActivatorOrigin() {
  const saved = window.localStorage.getItem("simpless-activator-origin");
  if (saved) {
    return saved;
  }

  if (window.location.port === "3000") {
    return window.location.origin;
  }

  return "http://127.0.0.1:3000";
}

function saveActivatorOrigin(value) {
  window.localStorage.setItem("simpless-activator-origin", normalizeOrigin(value));
}

function setStatus(node, message, tone) {
  if (!node) {
    return;
  }

  node.textContent = message;
  node.classList.remove("status-tone-good", "status-tone-warn", "status-tone-bad");
  if (tone === "good") {
    node.classList.add("status-tone-good");
  } else if (tone === "bad") {
    node.classList.add("status-tone-bad");
  } else {
    node.classList.add("status-tone-warn");
  }
}

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function escapeSql(value) {
  return String(value).replace(/'/g, "''");
}

function formatBytes(bytes) {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  if (bytes < 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatDuration(milliseconds) {
  if (milliseconds < 1000) {
    return `${milliseconds} ms ago`;
  }
  if (milliseconds < 60_000) {
    return `${(milliseconds / 1000).toFixed(1)} s ago`;
  }
  if (milliseconds < 3_600_000) {
    return `${(milliseconds / 60_000).toFixed(1)} min ago`;
  }
  return `${(milliseconds / 3_600_000).toFixed(1)} h ago`;
}

function formatTimestamp(unixMs) {
  return new Date(unixMs).toLocaleString();
}

function formatCell(value) {
  if (value === null || value === undefined) {
    return "NULL";
  }
  if (typeof value === "object") {
    return JSON.stringify(value);
  }
  return String(value);
}
