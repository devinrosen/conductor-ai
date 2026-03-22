import { useState, useEffect, useCallback } from "react";
import { defaultTheme, getThemeById, type Theme, type ThemePalette } from "./themes";

const STORAGE_KEY = "conductor-theme";

function applyPalette(palette: ThemePalette) {
  const root = document.documentElement;
  root.style.setProperty("--color-gray-50", palette.gray50);
  root.style.setProperty("--color-gray-100", palette.gray100);
  root.style.setProperty("--color-gray-200", palette.gray200);
  root.style.setProperty("--color-gray-300", palette.gray300);
  root.style.setProperty("--color-gray-400", palette.gray400);
  root.style.setProperty("--color-gray-500", palette.gray500);
  root.style.setProperty("--color-gray-600", palette.gray600);
  root.style.setProperty("--color-gray-700", palette.gray700);
  root.style.setProperty("--color-gray-800", palette.gray800);
  root.style.setProperty("--color-gray-900", palette.gray900);
  root.style.setProperty("--color-gray-950", palette.gray950);
  root.style.setProperty("--color-white", palette.white);

  // Map accent colors into the indigo + blue scales used by components
  root.style.setProperty("--color-indigo-100", palette.accentBg);
  root.style.setProperty("--color-indigo-500", palette.accent);
  root.style.setProperty("--color-indigo-600", palette.accentGlow);
  root.style.setProperty("--color-indigo-700", palette.accentGlow);
  root.style.setProperty("--color-blue-600", palette.accent);
  root.style.setProperty("--color-blue-700", palette.accent);

  // Signal status colors
  root.style.setProperty("--color-green-500", palette.statusGo);
  root.style.setProperty("--color-green-600", palette.statusGo);
  root.style.setProperty("--color-green-700", palette.statusGo);
  root.style.setProperty("--color-yellow-500", palette.statusCaution);
  root.style.setProperty("--color-yellow-600", palette.statusCaution);
  root.style.setProperty("--color-red-500", palette.statusStop);
  root.style.setProperty("--color-red-600", palette.statusStop);
  root.style.setProperty("--color-red-700", palette.statusStop);

  // Body background
  root.style.backgroundColor = palette.gray50;
  root.style.color = palette.gray800;
}

function loadSavedTheme(): Theme {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved) {
      const theme = getThemeById(saved);
      if (theme) return theme;
    }
  } catch {
    // localStorage unavailable
  }
  return defaultTheme;
}

export function useTheme() {
  const [theme, setThemeState] = useState<Theme>(loadSavedTheme);

  // Apply palette on mount and when theme changes
  useEffect(() => {
    applyPalette(theme.palette);
  }, [theme]);

  const setTheme = useCallback((t: Theme) => {
    setThemeState(t);
    try {
      localStorage.setItem(STORAGE_KEY, t.id);
    } catch {
      // localStorage unavailable
    }
  }, []);

  return { theme, setTheme };
}

/**
 * Apply the saved theme immediately on app startup, before React renders.
 * Call this in main.tsx to avoid a flash of the default theme.
 */
export function applyInitialTheme() {
  const theme = loadSavedTheme();
  applyPalette(theme.palette);
}
