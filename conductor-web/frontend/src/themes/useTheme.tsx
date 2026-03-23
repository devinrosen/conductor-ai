import { useState, useEffect, useCallback, createContext, useContext } from "react";
import { defaultTheme, getThemeById, type Theme } from "./themes";

const STORAGE_KEY = "conductor-theme";

// ── React context for theme ID ──
// Components that need to re-render on theme change use useThemeId().
const ThemeIdContext = createContext<string>(defaultTheme.id);
let globalSetThemeId: ((id: string) => void) | null = null;

export function ThemeIdProvider({ children }: { children: React.ReactNode }) {
  const [themeId, setThemeId] = useState(() => loadSavedTheme().id);
  globalSetThemeId = setThemeId;
  return (
    <ThemeIdContext.Provider value={themeId}>
      {children}
    </ThemeIdContext.Provider>
  );
}

/** Get the current theme ID reactively — re-renders on theme change. */
export function useThemeId(): string {
  return useContext(ThemeIdContext);
}

// ── Apply theme to DOM ──

function applyTheme(theme: Theme) {
  const root = document.documentElement;
  root.setAttribute("data-theme", theme.id);

  const { palette, typography, surfaces, spacing, motion } = theme;
  const s = root.style;

  s.setProperty("--color-gray-50", palette.gray50);
  s.setProperty("--color-gray-100", palette.gray100);
  s.setProperty("--color-gray-200", palette.gray200);
  s.setProperty("--color-gray-300", palette.gray300);
  s.setProperty("--color-gray-400", palette.gray400);
  s.setProperty("--color-gray-500", palette.gray500);
  s.setProperty("--color-gray-600", palette.gray600);
  s.setProperty("--color-gray-700", palette.gray700);
  s.setProperty("--color-gray-800", palette.gray800);
  s.setProperty("--color-gray-900", palette.gray900);
  s.setProperty("--color-gray-950", palette.gray950);
  s.setProperty("--color-white", palette.white);
  s.setProperty("--color-indigo-100", palette.accentBg);
  s.setProperty("--color-indigo-500", palette.accent);
  s.setProperty("--color-indigo-600", palette.accentGlow);
  s.setProperty("--color-indigo-700", palette.accentGlow);
  s.setProperty("--color-blue-600", palette.accent);
  s.setProperty("--color-blue-700", palette.accent);
  s.setProperty("--color-green-500", palette.statusGo);
  s.setProperty("--color-green-600", palette.statusGo);
  s.setProperty("--color-green-700", palette.statusGo);
  s.setProperty("--color-yellow-500", palette.statusCaution);
  s.setProperty("--color-yellow-600", palette.statusCaution);
  s.setProperty("--color-red-500", palette.statusStop);
  s.setProperty("--color-red-600", palette.statusStop);
  s.setProperty("--color-red-700", palette.statusStop);
  s.setProperty("--cd-heading-family", typography.headingFamily);
  s.setProperty("--cd-heading-weight", typography.headingWeight);
  s.setProperty("--cd-heading-letter-spacing", typography.headingLetterSpacing);
  s.setProperty("--cd-body-family", typography.bodyFamily);
  s.setProperty("--cd-body-size", typography.bodySize);
  s.setProperty("--cd-body-line-height", typography.bodyLineHeight);
  s.setProperty("--cd-label-letter-spacing", typography.labelLetterSpacing);
  s.setProperty("--cd-code-family", typography.codeFamily);
  s.setProperty("--font-sans", typography.bodyFamily);
  s.setProperty("--font-mono", typography.codeFamily);
  s.setProperty("--cd-radius-card", surfaces.borderRadiusCard);
  s.setProperty("--cd-radius-button", surfaces.borderRadiusButton);
  s.setProperty("--cd-radius-badge", surfaces.borderRadiusBadge);
  s.setProperty("--cd-border-style", surfaces.borderStyle);
  s.setProperty("--cd-card-padding", spacing.cardPadding);
  s.setProperty("--cd-item-gap", spacing.itemGap);
  s.setProperty("--cd-section-gap", spacing.sectionGap);
  s.setProperty("--cd-transition-duration", motion.transitionDuration);
  s.setProperty("--cd-transition-easing", motion.transitionEasing);

  s.backgroundColor = palette.gray50;
  s.color = palette.gray800;
  s.fontFamily = typography.bodyFamily;
  s.fontSize = typography.bodySize;
  s.lineHeight = typography.bodyLineHeight;
  if (typography.fontFeatures) {
    s.fontFeatureSettings = typography.fontFeatures;
  } else {
    s.removeProperty("font-feature-settings");
  }
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

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  const setTheme = useCallback((t: Theme) => {
    setThemeState(t);
    // Update the context so all useThemeId() consumers re-render
    globalSetThemeId?.(t.id);
    try {
      localStorage.setItem(STORAGE_KEY, t.id);
    } catch {
      // localStorage unavailable
    }
  }, []);

  return { theme, setTheme };
}

export function applyInitialTheme() {
  const theme = loadSavedTheme();
  applyTheme(theme);
}
