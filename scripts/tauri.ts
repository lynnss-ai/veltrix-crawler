import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const projectRoot = resolve(import.meta.dir, "..");
const cliArgs = Bun.argv.slice(2);

const packageJsonPath = resolve(projectRoot, "package.json");
const tauriConfigPath = resolve(projectRoot, "src-tauri", "tauri.conf.json");
const cargoManifestPath = resolve(projectRoot, "src-tauri", "Cargo.toml");
const cargoLockPath = resolve(projectRoot, "Cargo.lock");
const versionFiles = [
  packageJsonPath,
  tauriConfigPath,
  cargoManifestPath,
  cargoLockPath,
];

function jsonVersion(filePath: string): string {
  const value = JSON.parse(readFileSync(filePath, "utf8")) as {
    version?: unknown;
  };
  if (typeof value.version !== "string") {
    throw new Error(`${filePath} 缺少有效的 version 字段`);
  }
  return value.version;
}

function cargoVersion(filePath: string): string {
  const source = readFileSync(filePath, "utf8");
  const match = source.match(
    /\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
  );
  if (!match) {
    throw new Error(`${filePath} 的 [package] 缺少有效版本号`);
  }
  return match[1];
}

function nextPatchVersion(version: string): string {
  const match = version.match(/^(\d+)\.(\d+)\.(\d+)$/);
  if (!match) {
    throw new Error(`版本号必须是 major.minor.patch 格式，当前为 ${version}`);
  }
  return `${match[1]}.${match[2]}.${Number(match[3]) + 1}`;
}

function replaceJsonVersion(
  filePath: string,
  currentVersion: string,
  nextVersion: string,
): void {
  const source = readFileSync(filePath, "utf8");
  const pattern = /("version"\s*:\s*")([^"]+)(")/;
  const updated = source.replace(pattern, (full, prefix, version, suffix) => {
    if (version !== currentVersion) return full;
    return `${prefix}${nextVersion}${suffix}`;
  });
  if (updated === source) {
    throw new Error(`${filePath} 版本号更新失败`);
  }
  writeFileSync(filePath, updated, "utf8");
}

function replaceCargoVersion(
  filePath: string,
  currentVersion: string,
  nextVersion: string,
): void {
  const source = readFileSync(filePath, "utf8");
  const pattern = /(\[package\][\s\S]*?^version\s*=\s*")([^"]+)(")/m;
  const updated = source.replace(pattern, (full, prefix, version, suffix) => {
    if (version !== currentVersion) return full;
    return `${prefix}${nextVersion}${suffix}`;
  });
  if (updated === source) {
    throw new Error(`${filePath} 版本号更新失败`);
  }
  writeFileSync(filePath, updated, "utf8");
}

function bumpBuildVersion(): void {
  const versions = [
    jsonVersion(packageJsonPath),
    jsonVersion(tauriConfigPath),
    cargoVersion(cargoManifestPath),
  ];
  const uniqueVersions = new Set(versions);
  if (uniqueVersions.size !== 1) {
    throw new Error(
      `打包前版本号不一致：package.json=${versions[0]}，tauri.conf.json=${versions[1]}，Cargo.toml=${versions[2]}`,
    );
  }

  const currentVersion = versions[0];
  const nextVersion = nextPatchVersion(currentVersion);
  replaceJsonVersion(packageJsonPath, currentVersion, nextVersion);
  replaceJsonVersion(tauriConfigPath, currentVersion, nextVersion);
  replaceCargoVersion(cargoManifestPath, currentVersion, nextVersion);
  console.log(`打包版本号：${currentVersion} → ${nextVersion}`);
}

function snapshotVersionFiles(): Map<string, string> {
  return new Map(
    versionFiles
      .filter((filePath) => existsSync(filePath))
      .map((filePath) => [filePath, readFileSync(filePath, "utf8")]),
  );
}

function restoreVersionFiles(snapshot: Map<string, string>): void {
  for (const [filePath, source] of snapshot) {
    writeFileSync(filePath, source, "utf8");
  }
}

// 版本必须在 Tauri CLI 读取配置前更新，才能作用于本次安装包。
// 查看帮助不属于真实打包，不能因此消耗版本号。
const shouldBumpVersion =
  cliArgs[0] === "build" &&
  !cliArgs.includes("--help") &&
  !cliArgs.includes("-h");
const versionSnapshot = shouldBumpVersion ? snapshotVersionFiles() : undefined;
if (versionSnapshot) {
  try {
    bumpBuildVersion();
  } catch (error) {
    restoreVersionFiles(versionSnapshot);
    throw error;
  }
}

const tauriExecutable = resolve(
  projectRoot,
  "node_modules",
  ".bin",
  process.platform === "win32" ? "tauri.exe" : "tauri",
);
if (!existsSync(tauriExecutable)) {
  throw new Error("未找到 Tauri CLI，请先运行 bun install");
}

const child = Bun.spawn([tauriExecutable, ...cliArgs], {
  cwd: projectRoot,
  stdin: "inherit",
  stdout: "inherit",
  stderr: "inherit",
});
const exitCode = await child.exited;
if (exitCode !== 0 && versionSnapshot) {
  restoreVersionFiles(versionSnapshot);
  console.error("打包未成功，版本号已恢复");
}
process.exit(exitCode);
