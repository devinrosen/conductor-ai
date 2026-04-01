/**
 * Validates that a URL is safe to use in an anchor href attribute.
 * Rejects javascript: protocol URLs and other potentially dangerous schemes.
 * @param url - The URL to validate
 * @returns true if the URL is safe, false otherwise
 */
export function isSafeUrl(url: string): boolean {
  try {
    const parsedUrl = new URL(url);
    // Allow http, https, mailto, and tel protocols
    const allowedProtocols = ['http:', 'https:', 'mailto:', 'tel:'];
    return allowedProtocols.includes(parsedUrl.protocol);
  } catch {
    // Invalid URL
    return false;
  }
}

/**
 * Returns a safe URL or undefined if the URL is not safe.
 * Use this when you need to conditionally render a link based on URL safety.
 * @param url - The URL to validate
 * @returns The original URL if safe, undefined otherwise
 */
export function getSafeUrl(url: string | undefined): string | undefined {
  if (!url) return undefined;
  return isSafeUrl(url) ? url : undefined;
}