export function short(value, length = 12) {
  if (!value) return '-';
  return value.length > length ? `${value.slice(0, length)}…` : value;
}

export function time(value) {
  if (!value) return '-';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export function bytes(value) {
  const amount = Number(value || 0);
  if (amount < 1024) return `${amount} B`;
  if (amount < 1024 ** 2) return `${(amount / 1024).toFixed(1)} KiB`;
  if (amount < 1024 ** 3) return `${(amount / 1024 ** 2).toFixed(1)} MiB`;
  return `${(amount / 1024 ** 3).toFixed(1)} GiB`;
}

export function title(value) {
  return String(value || '')
    .replaceAll('.', ' ')
    .replaceAll('-', ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}
