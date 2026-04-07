/// <reference lib="webworker" />

import { precacheAndRoute, cleanupOutdatedCaches } from 'workbox-precaching';

// Type definitions for service worker events
declare const self: ServiceWorkerGlobalScope;

// Precache all static assets
precacheAndRoute(self.__WB_MANIFEST);
cleanupOutdatedCaches();

interface PushPayload {
  title: string;
  body: string;
  tag?: string;
  url?: string;
}

// Handle push notifications
self.addEventListener('push', (event: PushEvent) => {
  if (!event.data) {
    return;
  }

  try {
    const payload: PushPayload = event.data.json();

    const notificationOptions: NotificationOptions = {
      body: payload.body,
      tag: payload.tag,
      icon: '/icon-192.svg',
      badge: '/favicon.svg',
      data: {
        url: payload.url,
      },
      requireInteraction: true,
    };

    event.waitUntil(
      self.registration.showNotification(payload.title, notificationOptions)
    );
  } catch (error) {
    console.error('Error handling push event:', error);
  }
});

// Handle notification clicks
self.addEventListener('notificationclick', (event: NotificationEvent) => {
  event.notification.close();

  if (event.action === 'dismiss') {
    return;
  }

  // Default action or 'open' action
  const url = event.notification.data?.url || '/';

  event.waitUntil(
    // Try to focus an existing conductor tab/window
    self.clients.matchAll({
      type: 'window',
      includeUncontrolled: true,
    }).then((clients) => {
      // Look for an existing conductor window
      for (const client of clients) {
        if (client.url.includes(self.location.origin)) {
          // Focus the existing window and navigate to the desired URL
          return client.focus().then(() => {
            if (url !== '/') {
              return client.navigate(url);
            }
          });
        }
      }

      // No existing window found, open a new one
      return self.clients.openWindow(url);
    })
  );
});

// Handle notification close events (optional)
self.addEventListener('notificationclose', (event: NotificationEvent) => {
  // Optional: track notification dismissals for analytics
  console.log('Notification closed:', event.notification.tag);
});

// Handle service worker activation
self.addEventListener('activate', (event: ExtendableEvent) => {
  event.waitUntil(
    Promise.all([
      // Clean up old caches
      cleanupOutdatedCaches(),
      // Take control of all clients immediately
      self.clients.claim(),
    ])
  );
});

// Handle service worker installation
self.addEventListener('install', (event: ExtendableEvent) => {
  // Skip waiting to activate the new service worker immediately
  event.waitUntil(self.skipWaiting());
});

// Handle background sync (optional future feature)
self.addEventListener('sync', (event: any) => {
  if (event.tag === 'background-sync') {
    event.waitUntil(
      // Handle background sync tasks
      console.log('Background sync triggered')
    );
  }
});

export {};