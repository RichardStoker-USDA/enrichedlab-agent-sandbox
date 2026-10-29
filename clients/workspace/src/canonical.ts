// RFC 8785 canonical JSON, as the broker digests its receipts (agent-sandbox `docs/result_tables.md`): object keys in
// sorted order, no whitespace, strings and numbers as JSON.stringify writes them. The workspace checks a column
// summary's output digest against this before it shows the summaries.
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object")
    return `{${Object.keys(value).sort().filter((k) => (value as Record<string, unknown>)[k] !== undefined)
      .map((k) => `${JSON.stringify(k)}:${canonicalJson((value as Record<string, unknown>)[k])}`).join(",")}}`;
  return JSON.stringify(value);
}

export async function sha256Hex(text: string): Promise<string> {
  return sha256OfBytes(new TextEncoder().encode(text));
}

export async function sha256OfBytes(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
