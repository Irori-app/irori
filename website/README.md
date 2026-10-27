# Irori website

The project's one-page site: Vue 3, Vite, Tailwind CSS 4 and shadcn-vue components
(`src/components/ui`, copied from the shadcn-vue registry and owned here).

```bash
pnpm install
pnpm dev      # http://localhost:5173
pnpm build    # type-checks, then writes dist/
```

## Deploying with Coolify

Create an application from this repository with the **Dockerfile** build pack, base
directory `/website`, port **80** and the domain **https://irori.app**. The image builds the site and serves `dist/`
with Caddy; Coolify's proxy adds TLS and the domain.

## Screenshots

`public/screenshots/` holds the app's screens, as WebP at 1440 and 2880 pixels wide,
dark and (`light-` prefix) light. They come from a release build running on a demo
library of made-up games with covers drawn for the purpose, so no store artwork or
personal data appears. Never replace them with captures of a real library or real
folder paths. `library.png` is the screenshot Linux software centres show
(`packaging/linux/*.metainfo.xml`).

Fonts are self-hosted from Fontsource (Latin subsets only): Zen Maru Gothic for
headings, Zen Kaku Gothic New for text and M PLUS 1 Code for commands and figures.
