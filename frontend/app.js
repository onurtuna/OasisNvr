// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

// State variables
let state = {
    cams: [],
    hlsPlayers: {}, // Map of camera_id -> Hls instance for Live View
    vodHls: null    // Hls instance for VOD player
};

// DOM Elements
const els = {
    navItems: document.querySelectorAll('.nav-item'),
    views: document.querySelectorAll('.view'),
    pageTitle: document.getElementById('page-title'),
    clock: document.getElementById('clock'),
    statusText: document.getElementById('system-status-text'),

    // Dashboard Stats
    dashCams: document.getElementById('dash-cams-count'),
    dashPools: document.getElementById('dash-pools-count'),
    dashUsage: document.getElementById('dash-pool-usage'),
    dashSegs: document.getElementById('dash-segments'),

    // Live View
    liveContainer: document.getElementById('live-cameras-container'),

    // Recordings View
    recCamSelect: document.getElementById('rec-camera-select'),
    recDateFrom: document.getElementById('rec-date-from'),
    recDateTo: document.getElementById('rec-date-to'),
    btnFetchSegs: document.getElementById('btn-fetch-segments'),
    segsList: document.getElementById('segments-list'),

    // VOD Player
    vodPlayer: document.getElementById('vod-player'),
    vodOverlay: document.getElementById('vod-overlay'),
    btnPlayVod: document.getElementById('btn-play-vod'),
    btnDownloadVod: document.getElementById('btn-download-vod'),

    // Configuration View
    configCamList: document.getElementById('config-cameras-list'),
    addCamForm: document.getElementById('add-camera-form'),
    newCamId: document.getElementById('new-cam-id'),
    newCamName: document.getElementById('new-cam-name'),
    newCamUrl: document.getElementById('new-cam-url'),
};

// Setup Clock
function updateClock() {
    const now = new Date();
    els.clock.textContent = now.toLocaleTimeString();
}
setInterval(updateClock, 1000);
updateClock();

// Navigation logic
els.navItems.forEach(item => {
    item.addEventListener('click', (e) => {
        e.preventDefault();

        // Update Nav
        els.navItems.forEach(nav => nav.classList.remove('active'));
        item.classList.add('active');

        // Update Title
        els.pageTitle.textContent = item.textContent.trim();

        // Switch View
        const targetId = item.getAttribute('data-target');
        els.views.forEach(view => view.classList.remove('active'));
        document.getElementById(targetId).classList.add('active');

        // Handle specific view logistics
        if (targetId === 'live-view') {
            refreshLiveCameras();
        } else if (targetId === 'dashboard-view') {
            fetchStatus();
        } else if (targetId === 'configuration-view') {
            renderConfigCameras();
        }
    });
});

// Init
async function init() {
    await fetchStatus();
    await fetchCameras();

    // Helper to format Date to YYYY-MM-DDTHH:MM in local time
    const toLocalISO = dt => {
        const pad = num => num.toString().padStart(2, '0');
        return `${dt.getFullYear()}-${pad(dt.getMonth() + 1)}-${pad(dt.getDate())}T${pad(dt.getHours())}:${pad(dt.getMinutes())}:${pad(dt.getSeconds())}`;
    };

    // Default Dates for VOD (last 2 hours)
    const to = new Date();
    const from = new Date(to.getTime() - (2 * 60 * 60 * 1000));

    els.recDateTo.value = toLocalISO(to);
    els.recDateFrom.value = toLocalISO(from);
}

// Fetch System Status
async function fetchStatus() {
    try {
        const res = await fetch('/api/status');
        if (!res.ok) throw new Error('Status fetch failed');
        const data = await res.json();

        els.dashCams.textContent = data.cameras.length;
        els.dashPools.textContent = data.pool_files;
        els.dashUsage.textContent = `Pool ${data.active_pool_idx}: ${data.active_pool_pct.toFixed(1)}%`;
        els.dashSegs.textContent = data.total_segments;

        els.statusText.textContent = "System Online";
        document.querySelector('.dot').classList.add('active');
    } catch (err) {
        console.error("Error fetching status:", err);
        els.statusText.textContent = "System Offline";
        document.querySelector('.dot').classList.remove('active');
    }
}

// Fetch Cameras
async function fetchCameras() {
    try {
        const res = await fetch('/api/cameras');
        if (!res.ok) throw new Error('Cameras fetch failed');
        const data = await res.json();
        state.cams = data.cameras || [];

        // Populate Selects with all (active and historical) cameras
        els.recCamSelect.innerHTML = '<option value="" disabled selected>Select a camera</option>' +
            state.cams.map(c => `<option value="${c.id}">${c.name || c.id}</option>`).join('');

        // Setup Live Grid only with active cameras
        setupLiveGrid();

        // Setup Dashboard Camera List
        const dashCamList = document.getElementById('dash-camera-list');
        if (dashCamList && state.cams.length > 0) {
            const activeDashboardCams = state.cams.filter(c => c.status === 'active');

            if (activeDashboardCams.length > 0) {
                dashCamList.innerHTML = activeDashboardCams.map(cam => `
                    <div class="segment-item" style="display: flex; justify-content: space-between; align-items: center; padding: 12px 16px; cursor: default;">
                        <div>
                            <div style="font-weight:600; font-size:1.05rem;">
                                ${cam.name || cam.id} <span style="color:var(--text-muted); font-size:0.85rem; font-weight:normal;">(${cam.id})</span>
                            </div>
                            <div style="font-size:0.85rem; color:var(--text-muted); margin-top:2px;">
                                ${cam.url || 'No URL available'}
                            </div>
                        </div>
                        <div>
                            <span style="display:inline-block; padding:4px 8px; border-radius:4px; font-size:0.75rem; font-weight:600; text-transform:uppercase; 
                                background: rgba(102, 187, 106, 0.15); 
                                color: var(--accent-success); 
                                border: 1px solid rgba(102, 187, 106, 0.3);">
                                ${cam.status}
                            </span>
                        </div>
                    </div>
                `).join('');
            } else {
                dashCamList.innerHTML = '<div class="empty-state">No active cameras configured</div>';
            }
        } else if (dashCamList) {
            dashCamList.innerHTML = '<div class="empty-state">No cameras configured</div>';
        }

    } catch (err) {
        console.error("Error fetching cameras:", err);
    }
}

// --- Live View Logic ---
function setupLiveGrid() {
    els.liveContainer.innerHTML = '';

    const activeCams = state.cams.filter(c => c.status === 'active');

    if (activeCams.length === 0) {
        els.liveContainer.innerHTML = '<div class="empty-state" style="grid-column: 1/-1;">No active cameras configured</div>';
        return;
    }

    activeCams.forEach(cam => {
        const card = document.createElement('div');
        card.className = 'camera-card';
        card.innerHTML = `
            <div class="camera-header">
                <h3>${cam.name || cam.id}</h3>
                <div class="live-badge">LIVE</div>
            </div>
            <div class="video-container">
                <video id="live-video-${cam.id}" autoplay muted playsinline></video>
            </div>
        `;
        els.liveContainer.appendChild(card);
    });
}

// Start HLS for all live cameras when view is active
function refreshLiveCameras() {
    // Clean up old players
    Object.values(state.hlsPlayers).forEach(p => p.destroy());
    state.hlsPlayers = {};

    const activeCams = state.cams.filter(c => c.status === 'active');

    activeCams.forEach(cam => {
        const video = document.getElementById(`live-video-${cam.id}`);
        if (!video) return;

        const src = `/api/hls/${cam.id}/live.m3u8`;

        if (Hls.isSupported()) {
            const hls = new Hls({ liveSyncDurationCount: 3, liveMaxLatencyDurationCount: 6 });
            hls.loadSource(src);
            hls.attachMedia(video);
            hls.on(Hls.Events.MANIFEST_PARSED, () => video.play().catch(e => console.log(e)));
            state.hlsPlayers[cam.id] = hls;
        } else if (video.canPlayType('application/vnd.apple.mpegurl')) {
            video.src = src;
            video.addEventListener('loadedmetadata', () => video.play().catch(e => console.log(e)));
        }
    });
}


// --- Recordings (VOD) Logic ---

let currentSegments = []; // Cache segments for the selected camera

// Auto-fetch Segments when camera is chosen
els.recCamSelect.addEventListener('change', async () => {
    const camId = els.recCamSelect.value;
    if (!camId) return;

    try {
        els.segsList.innerHTML = '<div class="empty-state">Looking for recordings...</div>';
        els.btnPlayVod.disabled = true;
        els.btnDownloadVod.disabled = true;

        const res = await fetch(`/api/list?camera=${encodeURIComponent(camId)}`);
        if (!res.ok) throw new Error('List fetch failed');
        const data = await res.json();

        currentSegments = data.segments || [];

        if (currentSegments.length > 0) {
            // Sort segments newest first for the listing
            currentSegments.sort((a, b) => new Date(b.start + 'Z') - new Date(a.start + 'Z'));

            // Set default range to the absolute latest available footage
            const lastDate = new Date(currentSegments[0].end + 'Z');
            const oneHourAgo = new Date(lastDate.getTime() - (60 * 60 * 1000));
            const absoluteFirst = new Date(currentSegments[currentSegments.length - 1].start + 'Z');
            const startDate = oneHourAgo > absoluteFirst ? oneHourAgo : absoluteFirst;

            const toLocalISO = dt => {
                const pad = num => num.toString().padStart(2, '0');
                return `${dt.getFullYear()}-${pad(dt.getMonth() + 1)}-${pad(dt.getDate())}T${pad(dt.getHours())}:${pad(dt.getMinutes())}:${pad(dt.getSeconds())}`;
            };

            els.recDateFrom.value = toLocalISO(startDate);
            els.recDateTo.value = toLocalISO(lastDate);
        }

        displaySegmentsList(currentSegments);
    } catch (err) {
        console.error("Error fetching list:", err);
        els.segsList.innerHTML = '<div class="empty-state">Failed to fetch recordings</div>';
    }
});

// Refresh button
els.btnFetchSegs.addEventListener('click', (e) => {
    const camId = els.recCamSelect.value;
    if (!camId) return;
    els.recCamSelect.dispatchEvent(new Event('change'));
});

function displaySegmentsList(segments) {
    if (!segments || segments.length === 0) {
        els.segsList.innerHTML = '<div class="empty-state">No segments found</div>';
        return;
    }

    // Segments are sorted
    segments.sort((a, b) => new Date(a.start + 'Z') - new Date(b.start + 'Z'));

    // Group segments into continuous blocks (e.g. gap > 5 minutes = new block)
    const blocks = [];
    let currentBlock = null;
    const GAP_THRESHOLD_MS = 5 * 60 * 1000;

    segments.forEach(seg => {
        const sTime = new Date(seg.start + 'Z');
        const eTime = new Date(seg.end + 'Z');
        const sizeBytes = seg.size_bytes;

        if (!currentBlock) {
            currentBlock = { start: sTime, end: eTime, size: sizeBytes, segCount: 1, pools: new Set([seg.pool_idx]) };
            blocks.push(currentBlock);
        } else {
            const gap = sTime - currentBlock.end;
            if (gap <= GAP_THRESHOLD_MS) {
                // Extend current block
                currentBlock.end = new Date(Math.max(currentBlock.end, eTime));
                currentBlock.size += sizeBytes;
                currentBlock.segCount++;
                currentBlock.pools.add(seg.pool_idx);
            } else {
                // New block
                currentBlock = { start: sTime, end: eTime, size: sizeBytes, segCount: 1, pools: new Set([seg.pool_idx]) };
                blocks.push(currentBlock);
            }
        }
    });

    els.segsList.innerHTML = '';
    els.btnPlayVod.disabled = false;
    els.btnDownloadVod.disabled = false;

    // Reverse to show newest blocks first
    blocks.reverse();

    blocks.forEach(block => {
        const dStrStart = block.start.toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'medium' });
        const dStrEnd = block.end.toLocaleTimeString(undefined, { timeStyle: 'medium' });

        // Format Pool List
        const poolArr = Array.from(block.pools).sort((a, b) => a - b);
        let poolText = poolArr.length > 3
            ? `Pools: ${poolArr[0]}..${poolArr[poolArr.length - 1]}`
            : `Pool(s): ${poolArr.join(', ')}`;

        const durationMin = Math.round((block.end - block.start) / 60000);

        const el = document.createElement('div');
        el.className = 'segment-item';
        el.innerHTML = `
            <div class="segment-time">${dStrStart} — ${dStrEnd}</div>
            <div class="segment-meta" style="margin-top:4px;">
                <span>${durationMin} min (${block.segCount} segs)</span>
                <span>${(block.size / (1024 * 1024)).toFixed(1)} MB</span>
            </div>
            <div class="segment-meta" style="margin-top:2px; font-size: 0.7rem; color: #888;">
                <span>${poolText}</span>
            </div>
        `;

        // Set inputs to block range exactly
        el.addEventListener('click', () => {
            const pad = num => num.toString().padStart(2, '0');
            const toLocalISO = dt => {
                const pad = num => num.toString().padStart(2, '0');
                return `${dt.getFullYear()}-${pad(dt.getMonth() + 1)}-${pad(dt.getDate())}T${pad(dt.getHours())}:${pad(dt.getMinutes())}:${pad(dt.getSeconds())}`;
            };

            els.recDateFrom.value = toLocalISO(block.start);
            els.recDateTo.value = toLocalISO(block.end);

            // UI Selection
            document.querySelectorAll('.segment-item').forEach(i => i.classList.remove('selected'));
            el.classList.add('selected');
        });

        els.segsList.appendChild(el);
    });
}


// Play VOD for Range
els.btnPlayVod.addEventListener('click', () => {
    const camId = els.recCamSelect.value;
    const fromVal = els.recDateFrom.value; // Local naive format from input: "2026-02-20T14:18"
    const toVal = els.recDateTo.value;

    if (!camId || !fromVal || !toVal) return;

    // Convert local datetime input string to UTC string for the API
    const dFrom = new Date(fromVal);
    const dTo = new Date(toVal);

    // toISOString returns "YYYY-MM-DDTHH:MM:SS.000Z", we slice the first 19 characters expected by API
    const fromFmt = dFrom.toISOString().slice(0, 19);
    const toFmt = dTo.toISOString().slice(0, 19);

    const src = `/api/hls/${encodeURIComponent(camId)}/vod.m3u8?from=${encodeURIComponent(fromFmt)}&to=${encodeURIComponent(toFmt)}`;

    els.vodOverlay.classList.add('hidden');

    if (state.vodHls) {
        state.vodHls.destroy();
        state.vodHls = null;
    }

    if (Hls.isSupported()) {
        state.vodHls = new Hls({ startFragPrefetch: true });
        state.vodHls.loadSource(src);
        state.vodHls.attachMedia(els.vodPlayer);
        state.vodHls.on(Hls.Events.MANIFEST_PARSED, () => els.vodPlayer.play().catch(e => console.log(e)));
        state.vodHls.on(Hls.Events.ERROR, (_, data) => {
            if (data.type === Hls.ErrorTypes.NETWORK_ERROR) {
                alert("Network error. Ensure segments exist for this range.");
            }
        });
    } else if (els.vodPlayer.canPlayType('application/vnd.apple.mpegurl')) {
        els.vodPlayer.src = src;
        els.vodPlayer.addEventListener('loadedmetadata', () => els.vodPlayer.play().catch(e => console.log(e)));
    }
});


// Export VOD (Download .ts)
els.btnDownloadVod.addEventListener('click', () => {
    const camId = els.recCamSelect.value;
    const fromVal = els.recDateFrom.value;
    const toVal = els.recDateTo.value;

    if (!camId || !fromVal || !toVal) return;

    const dFrom = new Date(fromVal);
    const dTo = new Date(toVal);

    const fromFmt = dFrom.toISOString().slice(0, 19);
    const toFmt = dTo.toISOString().slice(0, 19);

    const url = `/api/export?camera=${encodeURIComponent(camId)}&from=${encodeURIComponent(fromFmt)}&to=${encodeURIComponent(toFmt)}`;
    window.location.href = url; // native browser download
});

// Boot
init();

// --- Configuration View Logic ---
function renderConfigCameras() {
    if (!els.configCamList) return;
    els.configCamList.innerHTML = '';

    const activeCams = state.cams.filter(c => c.status === 'active');

    if (activeCams.length === 0) {
        els.configCamList.innerHTML = '<div class="empty-state">No cameras configured</div>';
        return;
    }

    activeCams.forEach(cam => {
        const item = document.createElement('div');
        item.className = 'segment-item';
        item.style.display = 'block';
        item.style.padding = '16px';
        item.style.cursor = 'default';
        item.innerHTML = `
            <div style="margin-bottom: 12px;">
                <div style="font-weight:600; font-size:1.1rem; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; margin-bottom: 4px;">
                    ${cam.name || cam.id} <span style="color:var(--text-muted); font-size:0.85rem; font-weight:normal;">(ID: ${cam.id})</span>
                </div>
                <div style="font-size:0.85rem; color:var(--text-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis;">
                    <strong>Adres:</strong> ${cam.url}
                </div>
            </div>
            <button class="btn btn-secondary btn-remove-cam" data-id="${cam.id}" data-name="${cam.name || cam.id}" style="width:100%; padding:8px 12px; background:rgba(239,83,80,0.15); color:var(--accent-danger); border: 1px solid rgba(239,83,80,0.3); justify-content:center;">
                <svg viewBox="0 0 24 24" width="16" height="16" stroke="currentColor" stroke-width="2" fill="none" stroke-linecap="round" stroke-linejoin="round" style="margin-right:6px;"><path d="M3 6h18"></path><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path><line x1="10" y1="11" x2="10" y2="17"></line><line x1="14" y1="11" x2="14" y2="17"></line></svg>
                Kamerayı Sil
            </button>
        `;
        els.configCamList.appendChild(item);
    });

    // Attach remove listeners
    document.querySelectorAll('.btn-remove-cam').forEach(btn => {
        btn.addEventListener('click', async (e) => {
            const camId = e.currentTarget.getAttribute('data-id');
            const camName = e.currentTarget.getAttribute('data-name');
            if (!confirm(`'${camName}' kamerasını silmek istediğinize emin misiniz?`)) return;

            try {
                const res = await fetch(`/api/cameras/${encodeURIComponent(camId)}`, { method: 'DELETE' });
                if (!res.ok) {
                    const data = await res.json();
                    throw new Error(data.error || 'Failed to remove camera');
                }
                // Refresh state
                await fetchCameras();
                fetchStatus();
                renderConfigCameras();
            } catch (err) {
                console.error(err);
                alert("Failed to remove camera: " + err.message);
            }
        });
    });
}

if (els.addCamForm) {
    els.addCamForm.addEventListener('submit', async (e) => {
        e.preventDefault();

        const payload = {
            id: els.newCamId.value.trim(),
            name: els.newCamName.value.trim(),
            url: els.newCamUrl.value.trim(),
        };

        try {
            const btn = els.addCamForm.querySelector('button[type="submit"]');
            const origText = btn.textContent;
            btn.textContent = 'Adding...';
            btn.disabled = true;

            const res = await fetch('/api/cameras', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(payload)
            });

            btn.textContent = origText;
            btn.disabled = false;

            if (!res.ok) {
                const data = await res.json();
                throw new Error(data.error || 'Failed to add camera');
            }

            // Success
            els.addCamForm.reset();
            await fetchCameras();
            fetchStatus();
            renderConfigCameras();
        } catch (err) {
            console.error(err);
            alert("Error adding camera: " + err.message);

            const btn = els.addCamForm.querySelector('button[type="submit"]');
            btn.textContent = 'Add Camera';
            btn.disabled = false;
        }
    });
}

