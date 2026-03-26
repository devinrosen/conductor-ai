import { useState, useEffect, useCallback } from 'react';
import { apiClient } from '../api/client';

export type PushNotificationPermission = 'default' | 'granted' | 'denied';

export interface UsePushNotificationsState {
  isSupported: boolean;
  permission: PushNotificationPermission;
  isSubscribed: boolean;
  isLoading: boolean;
  error: string | null;
}

export interface UsePushNotificationsActions {
  requestPermission: () => Promise<boolean>;
  subscribe: () => Promise<boolean>;
  unsubscribe: () => Promise<boolean>;
  checkSubscription: () => Promise<void>;
}

export interface UsePushNotificationsReturn extends UsePushNotificationsState {
  actions: UsePushNotificationsActions;
}

/**
 * React hook for managing push notification subscriptions
 */
export function usePushNotifications(): UsePushNotificationsReturn {
  const [state, setState] = useState<UsePushNotificationsState>({
    isSupported: false,
    permission: 'default',
    isSubscribed: false,
    isLoading: true,
    error: null,
  });

  // Check if push notifications are supported
  const checkSupport = useCallback(() => {
    const isSupported = 'serviceWorker' in navigator &&
                       'PushManager' in window &&
                       'Notification' in window;

    setState(prev => ({
      ...prev,
      isSupported,
    }));

    return isSupported;
  }, []);

  // Get current permission state
  const checkPermission = useCallback((): PushNotificationPermission => {
    if (!('Notification' in window)) {
      return 'denied';
    }
    return Notification.permission as PushNotificationPermission;
  }, []);

  // Check current subscription status
  const checkSubscription = useCallback(async () => {
    setState(prev => ({ ...prev, isLoading: true, error: null }));

    try {
      if (!state.isSupported) {
        setState(prev => ({ ...prev, isSubscribed: false, isLoading: false }));
        return;
      }

      const registration = await navigator.serviceWorker.ready;
      const subscription = await registration.pushManager.getSubscription();

      setState(prev => ({
        ...prev,
        isSubscribed: !!subscription,
        isLoading: false,
      }));
    } catch (error) {
      console.error('Error checking push subscription:', error);
      setState(prev => ({
        ...prev,
        error: 'Failed to check push notification status',
        isLoading: false,
      }));
    }
  }, [state.isSupported]);

  // Request notification permission
  const requestPermission = useCallback(async (): Promise<boolean> => {
    setState(prev => ({ ...prev, isLoading: true, error: null }));

    try {
      if (!state.isSupported) {
        setState(prev => ({
          ...prev,
          error: 'Push notifications are not supported on this device',
          isLoading: false,
        }));
        return false;
      }

      const permission = await Notification.requestPermission();
      setState(prev => ({
        ...prev,
        permission: permission as PushNotificationPermission,
        isLoading: false,
      }));

      return permission === 'granted';
    } catch (error) {
      console.error('Error requesting notification permission:', error);
      setState(prev => ({
        ...prev,
        error: 'Failed to request notification permission',
        isLoading: false,
      }));
      return false;
    }
  }, [state.isSupported]);

  // Subscribe to push notifications
  const subscribe = useCallback(async (): Promise<boolean> => {
    setState(prev => ({ ...prev, isLoading: true, error: null }));

    try {
      if (!state.isSupported) {
        setState(prev => ({
          ...prev,
          error: 'Push notifications are not supported on this device',
          isLoading: false,
        }));
        return false;
      }

      const permission = checkPermission();
      if (permission !== 'granted') {
        const granted = await requestPermission();
        if (!granted) {
          setState(prev => ({
            ...prev,
            error: 'Permission denied for push notifications',
            isLoading: false,
          }));
          return false;
        }
      }

      // Get VAPID public key
      const vapidResponse = await apiClient.getPushVapidKey();
      const vapidPublicKey = vapidResponse.public_key;

      // Get service worker registration
      const registration = await navigator.serviceWorker.ready;

      // Subscribe to push notifications
      const applicationServerKey = urlBase64ToUint8Array(vapidPublicKey);
      const subscription = await registration.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: applicationServerKey as BufferSource,
      });

      // Send subscription to server
      await apiClient.subscribePush({
        endpoint: subscription.endpoint,
        keys: {
          p256dh: arrayBufferToBase64(subscription.getKey('p256dh')!),
          auth: arrayBufferToBase64(subscription.getKey('auth')!),
        },
      });

      setState(prev => ({
        ...prev,
        isSubscribed: true,
        permission: 'granted',
        isLoading: false,
      }));

      return true;
    } catch (error) {
      console.error('Error subscribing to push notifications:', error);
      setState(prev => ({
        ...prev,
        error: 'Failed to subscribe to push notifications',
        isLoading: false,
      }));
      return false;
    }
  }, [state.isSupported, checkPermission, requestPermission]);

  // Unsubscribe from push notifications
  const unsubscribe = useCallback(async (): Promise<boolean> => {
    setState(prev => ({ ...prev, isLoading: true, error: null }));

    try {
      const registration = await navigator.serviceWorker.ready;
      const subscription = await registration.pushManager.getSubscription();

      if (!subscription) {
        setState(prev => ({
          ...prev,
          isSubscribed: false,
          isLoading: false,
        }));
        return true;
      }

      // Unsubscribe from browser
      await subscription.unsubscribe();

      // Remove subscription from server
      await apiClient.unsubscribePush({
        endpoint: subscription.endpoint,
        keys: {
          p256dh: arrayBufferToBase64(subscription.getKey('p256dh')!),
          auth: arrayBufferToBase64(subscription.getKey('auth')!),
        },
      });

      setState(prev => ({
        ...prev,
        isSubscribed: false,
        isLoading: false,
      }));

      return true;
    } catch (error) {
      console.error('Error unsubscribing from push notifications:', error);
      setState(prev => ({
        ...prev,
        error: 'Failed to unsubscribe from push notifications',
        isLoading: false,
      }));
      return false;
    }
  }, []);

  // Initialize on mount
  useEffect(() => {
    const initialize = async () => {
      checkSupport();
      const permission = checkPermission();
      setState(prev => ({ ...prev, permission }));

      if (permission === 'granted') {
        await checkSubscription();
      } else {
        setState(prev => ({ ...prev, isLoading: false }));
      }
    };

    initialize();
  }, [checkSupport, checkPermission, checkSubscription]);

  const actions: UsePushNotificationsActions = {
    requestPermission,
    subscribe,
    unsubscribe,
    checkSubscription,
  };

  return {
    ...state,
    actions,
  };
}

// Utility functions

function urlBase64ToUint8Array(base64String: string): Uint8Array {
  const padding = '='.repeat((4 - base64String.length % 4) % 4);
  const base64 = (base64String + padding)
    .replace(/-/g, '+')
    .replace(/_/g, '/');

  const rawData = window.atob(base64);
  const outputArray = new Uint8Array(new ArrayBuffer(rawData.length));

  for (let i = 0; i < rawData.length; ++i) {
    outputArray[i] = rawData.charCodeAt(i);
  }

  return outputArray;
}

function arrayBufferToBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = '';
  for (let i = 0; i < bytes.byteLength; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return window.btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
}