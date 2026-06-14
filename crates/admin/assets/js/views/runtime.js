/**
 * Firefly Admin — Process & Runtime View.
 *
 * Displays real-time process metrics for the Rust/Tokio runtime: resident
 * memory (RSS), Tokio worker threads, live (alive) async tasks, and the
 * toolchain triple. Memory and task counts are tracked on a 5-minute rolling
 * line chart (60 data points at the configured refresh interval).
 *
 * Data source: GET /admin/api/runtime + SSE /runtime (event: runtime).
 * The backend emits `{ timestamp(ms), memory:{rss_mb,vms_mb},
 * tokio:{worker_threads,alive_tasks}, cpu:{logical_cores}, rust:{version,os,arch} }`.
 */

import { createLineChart } from '../charts.js';
import { createEmptyStateCard } from '../components/empty-state.js';
import { pageSkeleton } from '../components/skeleton.js';
import { sse } from '../sse.js';

/* ── Constants ──────────────────────────────────────────────── */

const MAX_DATA_POINTS = 60;

/* ── Helpers ────────────────────────────────────────────────── */

/**
 * Format a Unix timestamp (milliseconds) to a locale time string.
 * Falls back to the current time if no timestamp is provided.
 * @param {number|undefined} timestampMs
 * @returns {string}
 */
function formatTimestamp(timestampMs) {
    if (timestampMs) {
        return new Date(timestampMs).toLocaleTimeString();
    }
    return new Date().toLocaleTimeString();
}

/**
 * Push a value into a rolling-window array, trimming to MAX_DATA_POINTS.
 * @param {Array} arr
 * @param {*} value
 */
function pushRolling(arr, value) {
    arr.push(value);
    if (arr.length > MAX_DATA_POINTS) {
        arr.shift();
    }
}

/**
 * Create a stat card element.
 * @param {{ label: string, value: string, subtitle?: string, iconClass?: string }} opts
 * @returns {HTMLElement}
 */
function createStatCard({ label, value, subtitle, iconClass = 'primary' }) {
    const card = document.createElement('div');
    card.className = 'stat-card';

    const content = document.createElement('div');
    content.className = 'stat-card-content';

    const valEl = document.createElement('div');
    valEl.className = 'stat-card-value';
    valEl.textContent = value != null ? String(value) : '--';
    content.appendChild(valEl);

    const labelEl = document.createElement('div');
    labelEl.className = 'stat-card-label';
    labelEl.textContent = label;
    content.appendChild(labelEl);

    if (subtitle) {
        const subEl = document.createElement('div');
        subEl.style.fontSize = '0.7rem';
        subEl.style.color = 'var(--admin-text-muted)';
        subEl.style.marginTop = '2px';
        subEl.textContent = subtitle;
        content.appendChild(subEl);
    }

    card.appendChild(content);

    const icon = document.createElement('div');
    icon.className = `stat-card-icon ${iconClass}`;
    card.appendChild(icon);

    return card;
}

/**
 * Build a chart card with a header title and a canvas element.
 * @param {string} title
 * @returns {{ card: HTMLElement, canvas: HTMLCanvasElement }}
 */
function buildChartCard(title) {
    const card = document.createElement('div');
    card.className = 'admin-card';

    const header = document.createElement('div');
    header.className = 'admin-card-header';
    const h3 = document.createElement('h3');
    h3.textContent = title;
    header.appendChild(h3);
    card.appendChild(header);

    const body = document.createElement('div');
    body.className = 'admin-card-body';
    body.style.height = '200px';
    const canvas = document.createElement('canvas');
    body.appendChild(canvas);
    card.appendChild(body);

    return { card, canvas };
}

/* ── Render ───────────────────────────────────────────────────── */

/**
 * Render the process & runtime monitoring view.
 * @param {HTMLElement} container
 * @param {import('../api.js').AdminAPI} api
 * @returns {Promise<function>} Cleanup function
 */
export async function render(container, api) {
    container.replaceChildren();

    const wrapper = document.createElement('div');
    wrapper.className = 'view-enter';

    // ── Page header ──────────────────────────────────────────
    const header = document.createElement('div');
    header.className = 'page-header';
    const headerLeft = document.createElement('div');
    const h1 = document.createElement('h1');
    h1.textContent = 'Process';
    headerLeft.appendChild(h1);
    const sub = document.createElement('div');
    sub.className = 'page-subtitle';
    sub.textContent = 'process.runtime';
    headerLeft.appendChild(sub);
    header.appendChild(headerLeft);
    wrapper.appendChild(header);

    // ── Loading ──────────────────────────────────────────────
    const loader = document.createElement('div');
    loader.appendChild(pageSkeleton({ stats: 4, rows: 2 }));
    wrapper.appendChild(loader);
    container.appendChild(wrapper);

    let data;
    try {
        data = await api.get('/runtime');
    } catch (err) {
        wrapper.removeChild(loader);
        wrapper.appendChild(createEmptyStateCard({
            icon: 'alert',
            tone: 'danger',
            title: 'Failed to load runtime data',
            text: err.message,
        }));
        return;
    }

    wrapper.removeChild(loader);

    const memory = data.memory || {};
    const tokio = data.tokio || {};
    const cpu = data.cpu || {};
    const rust = data.rust || {};

    // ── Stat cards row (grid-4) ──────────────────────────────
    const statsRow = document.createElement('div');
    statsRow.className = 'grid-4 mb-lg';

    const memoryValueEl = createStatCard({
        label: 'Memory RSS',
        value: memory.rss_mb != null ? `${memory.rss_mb.toFixed(1)} MB` : '--',
        subtitle: memory.vms_mb != null ? `${memory.vms_mb.toFixed(0)} MB virtual` : undefined,
        iconClass: 'primary',
    });
    statsRow.appendChild(memoryValueEl);

    const threadsValueEl = createStatCard({
        label: 'Worker Threads',
        value: tokio.worker_threads != null ? String(tokio.worker_threads) : '--',
        subtitle: cpu.logical_cores != null ? `${cpu.logical_cores} cores` : undefined,
        iconClass: 'info',
    });
    statsRow.appendChild(threadsValueEl);

    const tasksValueEl = createStatCard({
        label: 'Active Tasks',
        value: tokio.alive_tasks != null ? String(tokio.alive_tasks) : '--',
        subtitle: 'tokio',
        iconClass: 'warning',
    });
    statsRow.appendChild(tasksValueEl);

    const rustValueEl = createStatCard({
        label: 'Rust',
        value: rust.version || '--',
        subtitle: (rust.os || rust.arch) ? `${rust.os || ''}/${rust.arch || ''}` : undefined,
        iconClass: 'success',
    });
    statsRow.appendChild(rustValueEl);

    wrapper.appendChild(statsRow);

    // References for updating stat card values from SSE
    const statValueEls = {
        memory: memoryValueEl.querySelector('.stat-card-value'),
        threads: threadsValueEl.querySelector('.stat-card-value'),
        tasks: tasksValueEl.querySelector('.stat-card-value'),
    };

    // ── Rolling data arrays ──────────────────────────────────
    const labels = [];
    const memoryData = [];
    const taskData = [];

    // Seed first data point from the initial fetch
    labels.push(formatTimestamp(data.timestamp));
    memoryData.push(memory.rss_mb || 0);
    taskData.push(tokio.alive_tasks || 0);

    // ── Chart cards (grid-2) ─────────────────────────────────
    const chartsRow = document.createElement('div');
    chartsRow.className = 'grid-2 mb-lg';

    const { card: memCard, canvas: memCanvas } = buildChartCard('Memory RSS (MB)');
    chartsRow.appendChild(memCard);

    const { card: taskCard, canvas: taskCanvas } = buildChartCard('Active Tasks');
    chartsRow.appendChild(taskCard);

    wrapper.appendChild(chartsRow);

    // ── Initialise charts after canvases are in the DOM ──────
    let memChart = null;
    let taskChart = null;

    requestAnimationFrame(() => {
        memChart = createLineChart(memCanvas, {
            label: 'Memory RSS (MB)',
            color: '--admin-primary',
            data: [...memoryData],
            labels: [...labels],
        });

        taskChart = createLineChart(taskCanvas, {
            label: 'Active Tasks',
            color: '--admin-info',
            data: [...taskData],
            labels: [...labels],
        });
    });

    // ── SSE real-time updates ────────────────────────────────
    sse.connectTyped('/runtime', 'runtime', (eventData) => {
        const label = formatTimestamp(eventData.timestamp);
        const mem = eventData.memory || {};
        const tk = eventData.tokio || {};

        pushRolling(labels, label);
        pushRolling(memoryData, mem.rss_mb || 0);
        pushRolling(taskData, tk.alive_tasks || 0);

        if (mem.rss_mb != null) {
            statValueEls.memory.textContent = `${mem.rss_mb.toFixed(1)} MB`;
        }
        if (tk.worker_threads != null) {
            statValueEls.threads.textContent = String(tk.worker_threads);
        }
        if (tk.alive_tasks != null) {
            statValueEls.tasks.textContent = String(tk.alive_tasks);
        }

        if (memChart) {
            memChart.update([...memoryData], [...labels]);
        }
        if (taskChart) {
            taskChart.update([...taskData], [...labels]);
        }
    });

    // ── Cleanup ──────────────────────────────────────────────
    return function cleanup() {
        sse.disconnect('/runtime');
        if (memChart) memChart.destroy();
        if (taskChart) taskChart.destroy();
    };
}
