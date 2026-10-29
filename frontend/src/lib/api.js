async function responseJson(response) {
  const payload = await response.json().catch(() => ({}));
  if (!response.ok || !payload.ok) {
    throw new Error(payload.error || `HTTP ${response.status}`);
  }
  return payload.data;
}

export async function loadDashboard() {
  return responseJson(await fetch('/api/v1/state', { cache: 'no-store' }));
}

export function stateStreamUrl() {
  return '/api/v1/state/stream';
}

export async function searchDashboard(query, scope = 'all', limit = 40, cursor = 0, signal) {
  const params = new URLSearchParams({
    q: query,
    scope,
    limit: String(limit),
    cursor: String(cursor)
  });
  return responseJson(
    await fetch(`/api/v1/search?${params}`, { cache: 'no-store', signal })
  );
}

export async function loadProvenanceStatus() {
  return responseJson(await fetch('/api/v1/provenance-status', { cache: 'no-store' }));
}

export async function loadGraph(run, limit = 5000) {
  const encoded = encodeURIComponent(run);
  return responseJson(
    await fetch(`/api/v1/provenance/${encoded}?limit=${limit}`, { cache: 'no-store' })
  );
}

export function graphStreamUrl(run, limit = 5000) {
  return `/api/v1/provenance/${encodeURIComponent(run)}/stream?limit=${limit}`;
}

export async function mutate(path, intent, body = {}) {
  return responseJson(
    await fetch(path, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Agent-Sandbox-Intent': intent
      },
      body: JSON.stringify(body)
    })
  );
}

export async function issueEvidenceLease(run, purpose) {
  return mutate(
    `/api/v1/provenance/${encodeURIComponent(run)}/lease`,
    'open-evidence',
    { purpose }
  );
}

export async function loadEventEvidence(run, eventId, token) {
  const response = await fetch(
    `/api/v1/provenance/${encodeURIComponent(run)}/event/${encodeURIComponent(eventId)}/evidence`,
    {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Agent-Sandbox-Intent': 'read-evidence',
        'X-Agent-Sandbox-Evidence-Lease': token
      },
      body: '{}'
    }
  );
  return responseJson(response);
}

export async function searchEvidence(run, query, token, limit = 25, cursor = 0) {
  const response = await fetch(
    `/api/v1/provenance/${encodeURIComponent(run)}/evidence/search`,
    {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Agent-Sandbox-Intent': 'search-evidence',
        'X-Agent-Sandbox-Evidence-Lease': token
      },
      body: JSON.stringify({ query, limit, cursor })
    }
  );
  return responseJson(response);
}

export async function downloadArtifact(run, artifact, token) {
  const response = await fetch(
    `/api/v1/provenance/${encodeURIComponent(run)}/artifact/${artifact.sha256}/content`,
    {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Agent-Sandbox-Intent': 'read-evidence',
        'X-Agent-Sandbox-Evidence-Lease': token
      },
      body: '{}'
    }
  );
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    throw new Error(payload.error || `HTTP ${response.status}`);
  }
  const blob = await response.blob();
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = `artifact-${artifact.sha256}.bin`;
  anchor.click();
  URL.revokeObjectURL(url);
}
