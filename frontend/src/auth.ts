const TOKEN_KEY = "dec_party_manager_token";
const REFRESH_TOKEN_KEY = "dec_party_manager_refresh_token";
const ID_TOKEN_KEY = "dec_party_manager_id_token";

export function getToken(): string | null {
  return sessionStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
  sessionStorage.setItem(TOKEN_KEY, token);
}

export function getRefreshToken(): string | null {
  return sessionStorage.getItem(REFRESH_TOKEN_KEY);
}

export function setRefreshToken(token: string): void {
  sessionStorage.setItem(REFRESH_TOKEN_KEY, token);
}

export function getIdToken(): string | null {
  return sessionStorage.getItem(ID_TOKEN_KEY);
}

export function setIdToken(token: string): void {
  sessionStorage.setItem(ID_TOKEN_KEY, token);
}

export function clearToken(): void {
  sessionStorage.removeItem(TOKEN_KEY);
  sessionStorage.removeItem(REFRESH_TOKEN_KEY);
  sessionStorage.removeItem(ID_TOKEN_KEY);
}
