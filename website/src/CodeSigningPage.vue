<script setup lang="ts">
import SiteFooter from '@/components/SiteFooter.vue'
import SiteHeader from '@/components/SiteHeader.vue'
import { REPO } from '@/data/content'

/** The packaging key: apt, dnf and pacman verify packages against it. */
const PACKAGES_KEY = '6DD7 B727 03FE 3BD5 C719  F4CE 6FE6 81AB 798D F656'
const MAC_IDENTITY = 'Developer ID Application: RADU MIHAI DIACONU (69QGT49729)'
</script>

<template>
  <SiteHeader />
  <main class="gutter mx-auto flex max-w-3xl flex-col gap-14 pb-24 pt-14">
    <header class="flex flex-col gap-4">
      <p class="tag">Code signing policy</p>
      <h1 class="font-display text-[clamp(2.2rem,5vw,3.4rem)] font-bold leading-[1.05] text-paper">
        How Irori’s releases are signed
      </h1>
      <p class="text-lg text-smoke">
        Every Irori download is built from public source by GitHub Actions and signed, so you can
        check that it comes from this project and hasn’t been changed.
      </p>
      <p class="signpath rounded-xl border border-grain bg-soot px-5 py-4 text-paper">
        Free code signing provided by
        <a href="https://about.signpath.io">SignPath.io</a>,
        certificate by
        <a href="https://signpath.org">SignPath Foundation</a>.
      </p>
    </header>

    <section aria-labelledby="signed" class="prose-section">
      <h2 id="signed">What is signed</h2>
      <ul>
        <li>
          <strong>Windows:</strong> <code>irori.exe</code>, <code>irori-sign-in.exe</code> and the
          installer, <code>Irori-X.Y.Z-windows-setup.exe</code>, in every release on the
          <a :href="`${REPO}/releases`">releases page</a>. They are signed through SignPath
          Foundation’s program for open-source projects; the project has applied, and Windows
          builds are unsigned until it is approved.
        </li>
        <li>
          <strong>macOS:</strong> <code>Irori.app</code> and its disk images are signed with the
          project’s Apple Developer ID and notarized by Apple.
        </li>
        <li>
          <strong>Linux:</strong> the apt, dnf and Arch repositories at
          <code>packages.irori.app</code> and the packages in them are signed with the Irori
          packages key.
        </li>
      </ul>
    </section>

    <section aria-labelledby="built" class="prose-section">
      <h2 id="built">How a release is built</h2>
      <ol>
        <li>
          The source is public at <a :href="REPO">github.com/irori-app/irori</a>. Every change
          reaches the <code>main</code> branch through a pull request, and <code>main</code>
          accepts it only after the tests pass on Linux, macOS and Windows.
        </li>
        <li>
          A release starts from a version tag on <code>main</code>. GitHub Actions builds every
          package from that commit with the
          <a :href="`${REPO}/blob/main/.github/workflows/release.yml`">release workflow</a>,
          installs and starts them on clean systems, then drafts the release with checksums and
          build attestations.
        </li>
        <li>
          Only files built by that workflow are signed. Nothing built on a personal computer is
          sent for signing, and each Windows signing request is approved by hand by an approver.
        </li>
      </ol>
    </section>

    <section aria-labelledby="team" class="prose-section">
      <h2 id="team">Who can change and sign Irori</h2>
      <div class="overflow-x-auto">
        <table>
          <thead>
            <tr><th scope="col">Role</th><th scope="col">Who</th></tr>
          </thead>
          <tbody>
            <tr>
              <td>Committers and reviewers</td>
              <td>Members of the <a href="https://github.com/irori-app">irori-app</a> GitHub organization: <a href="https://github.com/countradooku">@countradooku</a></td>
            </tr>
            <tr>
              <td>Approvers</td>
              <td>Owners of the irori-app organization: <a href="https://github.com/countradooku">@countradooku</a></td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section aria-labelledby="privacy" class="prose-section">
      <h2 id="privacy">Privacy</h2>
      <p>
        Irori sends no telemetry, analytics or crash reports. When it crashes, the report is saved
        on your computer and nowhere else.
      </p>
      <p>Without you asking, Irori contacts only these:</p>
      <ul>
        <li>
          <code>www.gstatic.com</code>, with an empty request at startup and once a minute, to
          know whether you are online. Downloads pause while you aren’t.
        </li>
        <li>
          <code>raw.githubusercontent.com</code> at startup, for public lists: anticheat
          compatibility, known game fixes, and Wine and Proton releases.
        </li>
        <li>
          The stores you have signed in to, at startup and then periodically, to refresh your
          library and check for game updates. You can turn this off in Settings.
        </li>
      </ul>
      <p>
        Everything else happens when you use a feature: signing in to a store, installing or
        updating a game, game artwork and details (ProtonDB, PCGamingWiki, HowLongToBeat,
        SteamGridDB), store deals, downloading Wine, Proton or an emulator, or uploading a log to
        dpaste.com when you choose to share it. Each request goes straight to that service, carries
        only what the feature needs, and is covered by that service’s own privacy policy.
      </p>
    </section>

    <section aria-labelledby="verify" class="prose-section">
      <h2 id="verify">Check a download</h2>
      <ul>
        <li>
          <strong>Windows:</strong> right-click the file, choose <em>Properties → Digital
          Signatures</em>, and check that the signer is SignPath Foundation.
        </li>
        <li>
          <strong>macOS:</strong> run <code>spctl -a -vv /Applications/Irori.app</code>. It should
          report <em>Notarized Developer ID</em> from <code>{{ MAC_IDENTITY }}</code>.
        </li>
        <li>
          <strong>Linux:</strong> apt, dnf and pacman check every package against the Irori
          packages key, fingerprint <code class="whitespace-nowrap">{{ PACKAGES_KEY }}</code>.
        </li>
        <li>
          <strong>Any file:</strong> compare it with <code>SHA256SUMS</code> in its release, or
          verify its build attestation with <code>gh attestation verify &lt;file&gt; -R irori-app/irori</code>.
        </li>
      </ul>
      <p>
        Found a signed file that doesn’t match, or something that looks wrong?
        <a :href="`${REPO}/issues`">Report it on GitHub</a>.
      </p>
    </section>
  </main>
  <SiteFooter />
</template>

<style scoped>
.signpath a,
.prose-section a {
  color: var(--paper);
  text-decoration: underline;
  text-decoration-color: var(--wood);
  text-underline-offset: 4px;
}
.signpath a:hover,
.prose-section a:hover {
  text-decoration-color: var(--flame);
}
.prose-section {
  display: flex;
  flex-direction: column;
  gap: 0.9rem;
  color: var(--smoke);
  max-width: 42rem;
}
.prose-section h2 {
  font-family: var(--font-display);
  font-size: clamp(1.5rem, 2.6vw, 1.9rem);
  font-weight: 700;
  color: var(--paper);
  margin-bottom: 0.2rem;
}
.prose-section strong {
  color: var(--paper);
  font-weight: 700;
}
.prose-section ul,
.prose-section ol {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
  padding-left: 1.25rem;
}
.prose-section ul {
  list-style: disc;
}
.prose-section ol {
  list-style: decimal;
}
.prose-section li::marker {
  color: var(--tatami);
}
.prose-section code {
  font-family: var(--font-code);
  font-size: 0.88em;
  padding: 0.05em 0.35em;
  border-radius: 0.35rem;
  background: var(--soot);
  color: var(--paper);
  overflow-wrap: anywhere;
}
.prose-section table {
  width: 100%;
  border-collapse: collapse;
  border: 1px solid var(--grain);
  border-radius: 0.75rem;
  overflow: hidden;
}
.prose-section th,
.prose-section td {
  text-align: left;
  padding: 0.75rem 1rem;
  border-bottom: 1px solid var(--grain);
  vertical-align: top;
}
.prose-section th {
  font-family: var(--font-code);
  font-size: 0.75rem;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--tatami);
  background: var(--soot);
}
.prose-section tr:last-child td {
  border-bottom: none;
}
.prose-section td:first-child {
  color: var(--paper);
  white-space: nowrap;
}
</style>
