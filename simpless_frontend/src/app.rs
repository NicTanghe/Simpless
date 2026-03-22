use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    StaticSegment,
    components::{Route, Router, Routes},
};

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <AutoReload options=options.clone() />
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
                <script type="module" src="/admin.js"></script>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Stylesheet id="leptos" href="/pkg/simpless_frontend.css"/>
        <Title text="simpless control deck"/>

        <Router>
            <main>
                <Routes fallback=|| "Page not found.".into_view()>
                    <Route path=StaticSegment("") view=HomePage/>
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn HomePage() -> impl IntoView {
    view! {
        <div class="dashboard-shell">
            <section class="hero-panel">
                <div class="hero-wash hero-wash-a" data-parallax-speed="-0.16"></div>
                <div class="hero-wash hero-wash-b" data-parallax-speed="0.1"></div>
                <div class="hero-grid" data-parallax-speed="0.04"></div>

                <div class="hero-topline">
                    <p class="hero-kicker">"simpless / control deck"</p>
                    <div class="hero-chip-row">
                        <span class="hero-chip">"parallax admin"</span>
                        <span class="hero-chip">"binary uploads"</span>
                        <span class="hero-chip">"live SQL edits"</span>
                    </div>
                </div>

                <div class="hero-copy" data-parallax-speed="0.14">
                    <p class="eyebrow">"upload binaries. reshape routes. wake services on demand."</p>
                    <h1>"A scrolling ops deck for the activator."</h1>
                    <p class="hero-lede">
                        "Point this frontend at the activator, drop in runnable binaries, and edit "
                        "service definitions directly against the SQLite registry."
                    </p>
                </div>

                <div class="hero-metrics">
                    <article class="metric-card" data-parallax-speed="0.2">
                        <span class="metric-label">"services"</span>
                        <strong id="service-count">"0"</strong>
                        <span class="metric-meta" id="config-path">"waiting for activator"</span>
                    </article>
                    <article class="metric-card metric-card-accent" data-parallax-speed="0.27">
                        <span class="metric-label">"running now"</span>
                        <strong id="running-count">"0"</strong>
                        <span class="metric-meta" id="overview-status">"connect to refresh"</span>
                    </article>
                    <article class="metric-card" data-parallax-speed="0.12">
                        <span class="metric-label">"uploaded binaries"</span>
                        <strong id="binary-count">"0"</strong>
                        <span class="metric-meta" id="upload-dir">"upload lane idle"</span>
                    </article>
                </div>
            </section>

            <section class="dock-panel">
                <div class="dock-copy">
                    <p class="section-tag">"Target Activator"</p>
                    <h2>"Keep the frontend separate, point it at the gateway."</h2>
                    <p>
                        "The frontend defaults to the local activator on "
                        <code>"http://127.0.0.1:3000"</code>
                        ". Change it here if the gateway is running somewhere else."
                    </p>
                </div>

                <form class="dock-form" id="connection-form">
                    <label class="field">
                        <span>"Activator origin"</span>
                        <input
                            id="activator-origin"
                            type="url"
                            name="activator_origin"
                            placeholder="http://127.0.0.1:3000"
                            autocomplete="off"
                        />
                    </label>
                    <button class="action-button" type="submit">"Refresh overview"</button>
                </form>
            </section>

            <section class="dashboard-grid">
                <article class="panel upload-panel">
                    <div class="panel-header">
                        <p class="section-tag">"Upload Lane"</p>
                        <h2>"Drop in runnable binaries"</h2>
                        <p>
                            "Files land in the activator upload directory. After upload, the UI gives you a SQL insert "
                            "template you can tweak before saving."
                        </p>
                    </div>

                    <form id="upload-form" class="stack-form">
                        <label class="field field-upload">
                            <span>"Binary files"</span>
                            <input id="binary-file" type="file" multiple=true/>
                        </label>
                        <button class="action-button action-button-bright" type="submit">"Upload to activator"</button>
                    </form>

                    <div class="status-bar" id="upload-status">"No uploads yet."</div>
                    <pre class="sql-preview" id="upload-sql-preview">
                        "Upload a binary to generate an INSERT template."
                    </pre>
                </article>

                <article class="panel services-panel">
                    <div class="panel-header">
                        <p class="section-tag">"Live Services"</p>
                        <h2>"Edit existing entries by filling the SQL editor"</h2>
                        <p>
                            "Each card exposes the exact command, port, and health route the activator is using. "
                            "Use the button on a card to load an UPDATE statement into the editor."
                        </p>
                    </div>

                    <div class="card-list" id="services-list">
                        <div class="empty-card">"No services loaded yet."</div>
                    </div>
                </article>

                <article class="panel sql-panel">
                    <div class="panel-header">
                        <p class="section-tag">"SQL Console"</p>
                        <h2>"Run one statement at a time against the registry"</h2>
                        <p>
                            "SELECT returns rows. INSERT, UPDATE, DELETE, and DDL validate the config inside a transaction "
                            "before the activator swaps to the new registry."
                        </p>
                    </div>

                    <form id="sql-form" class="stack-form">
                        <label class="field">
                            <span>"SQLite statement"</span>
                            <textarea id="sql-editor" spellcheck="false">
SELECT route_prefix, command, backend_port, idle_timeout_secs
FROM services
ORDER BY route_prefix;
                            </textarea>
                        </label>
                        <button class="action-button" type="submit">"Run SQL"</button>
                    </form>

                    <div class="status-bar" id="sql-status">"Ready."</div>
                    <div class="sql-result" id="sql-result"></div>
                </article>

                <article class="panel binaries-panel">
                    <div class="panel-header">
                        <p class="section-tag">"Binary Shelf"</p>
                        <h2>"Turn uploaded files into service rows"</h2>
                        <p>
                            "Use a binary card to push an INSERT template into the editor, then adjust the route prefix and "
                            "port before you run it."
                        </p>
                    </div>

                    <div class="card-list" id="binaries-list">
                        <div class="empty-card">"No uploaded binaries in the activator yet."</div>
                    </div>
                </article>
            </section>
        </div>
    }
}
