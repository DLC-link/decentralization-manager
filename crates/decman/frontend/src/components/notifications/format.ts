const formatRelativeTime = (epochSeconds: number): string => {
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - epochSeconds);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
};

const formatTimeUntil = (epochSeconds: number): string => {
  const seconds = Math.max(0, epochSeconds - Math.floor(Date.now() / 1000));
  if (seconds < 60) return "in under a minute";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `in ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `in ${hours}h`;
  const days = Math.floor(hours / 24);
  return `in ${days}d`;
};

const truncatePartyId = (id: string): string => {
  const parts = id.split("::");
  if (parts.length !== 2) return id;
  const [prefix, namespace] = parts;
  return `${prefix}::${namespace.slice(0, 6)}…${namespace.slice(-6)}`;
};

const truncateKey = (key: string): string =>
  key.length > 20 ? `${key.slice(0, 10)}…${key.slice(-8)}` : key;

export { formatRelativeTime, formatTimeUntil, truncatePartyId, truncateKey };
