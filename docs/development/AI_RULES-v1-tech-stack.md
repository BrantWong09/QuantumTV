# QuantumTV AI Development Rules

This document outlines the technical stack and development guidelines for the QuantumTV application.

## 🛠 Tech Stack

- **Next.js 16 (App Router)**: Used as the frontend framework, configured for static export (`output: 'export'`) to integrate with Tauri.
- **Tauri 2.0**: The cross-platform bridge providing access to the system (Rust core), window management, and native APIs.
- **React 19**: Modern UI component library using Functional Components and Hooks.
- **Tailwind CSS 4**: Primary styling utility for all components, utilizing the "Cinematic Aurora" design system.
- **TypeScript**: Strictly used for type safety across the frontend.
- **Rust**: Used for the backend core, handling data fusion, network requests (bypassing CORS), and performance-critical logic.
- **Plyr & HLS.js**: The core video playback engine supporting M3U8 streaming and custom player controls.
- **SQLite**: Local-first database managed via Rust for storing history, favorites, and settings.
- **Lucide React**: The standard library for all iconography.

## 📐 Development Rules

### 1. Component Structure
- **Location**: All UI components go into `src/components/`. Pages go into `src/app/`.
- **Granularity**: Keep components small and focused (ideally under 150 lines).
- **Client Directives**: Use `'use client';` at the top of files that utilize React hooks or browser APIs.

### 2. Styling Guidelines
- **Utility First**: Always use Tailwind CSS classes. Avoid custom CSS unless implementing complex keyframe animations in `globals.css`.
- **Theming**: Support both Light and Dark modes using `next-themes`. Use the `dark:` prefix for dark mode overrides.
- **Responsiveness**: Always build with a mobile-first approach. Ensure layouts work on 375px (Phone), 834px (Tablet), and 1440px+ (Desktop).

### 3. Library Usage Rules
- **Icons**: Exclusively use `lucide-react`. Do not add other icon libraries.
- **Navigation**: Use the custom `FastLink` component instead of native `next/link` for performance-critical navigation (especially in the sidebar/navbar).
- **Data Fetching**: Use `invoke` from `@tauri-apps/api/core` for backend calls. Do not use standard `fetch` for resources that might have CORS restrictions; let the Rust core handle them.
- **Images**: Use the `useProxyImage` hook for movie posters to handle image proxying and SQLite caching automatically.
- **State**: Use React state/hooks for local UI state. Use the Tauri event system (`listen`, `emit`) for cross-window or backend-to-frontend synchronization.

### 4. Code Quality
- **Simplicity**: Favor simple, readable code over complex abstractions.
- **No Placeholders**: Never output TODOs or partial implementations.
- **Error Handling**: Allow errors to bubble up to the `GlobalErrorIndicator` unless local recovery is specifically required.

### 5. Deployment Context
- Remember that this app is a **static export**. Features relying on Node.js server-side runtimes (like Middleware or Server Actions) will not work. All "server" logic must reside in the Rust `src-tauri` directory.