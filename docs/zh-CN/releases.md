# 二进制发布

[English](../releases.md) | 简体中文

Release 工作流在原生 x86_64 和 ARM64 runner 上编译，随后将压缩包实际安装为
Nix 包，运行输入、界面与 Rime 测试。推送版本 tag 后自动发布验证过的产物；
手动运行工作流或推送到 `release/` 分支只生成 Actions 附件，不公开 Release。

新的发布构建还生成 deb、RPM、AppImage 和 AUR 配方，并在隔离容器内检查安装。
各格式的安装方式，以及为已有版本打包的独立工作流，见 [Linux 安装包指南](https://github.com/Ign1x/oh-my-ime/blob/main/docs/zh-CN/linux-packages.md)。

从 [v0.4.0](https://github.com/Ign1x/oh-my-ime/releases/tag/v0.4.0) 起提供这些附件，
下面的下载地址现已可用。更早的 v0.1.0–v0.3.0 tag 没有二进制附件。

## NixOS 与 Home Manager

将输入源改为 Release 中的 flake 附件：

```nix
inputs.oh-my-ime.url =
  "https://github.com/Ign1x/oh-my-ime/releases/latest/download/oh-my-ime-nix.tar.gz";
```

`flake.lock` 仍固定实际版本。以后更新：

```sh
nix flake update oh-my-ime
sudo nixos-rebuild switch --flake .
```

如需指定版本，将 `latest/download` 替换为 `download/vX.Y.Z`。
继续导入 `inputs.oh-my-ime.nixosModules.default` 或
`inputs.oh-my-ime.homeManagerModules.default`，接口不变。包默认改为下载带固定
SHA-256 校验值的平台与 Rime 二进制，Nix 负责修补动态加载器和库路径、安装清单、
生成启动包装，不运行 Cargo。运行依赖仍由所选 nixpkgs 提供。

如果配置中覆写了 `programs.oh-my-ime.package` 或 `rime.package`，需要移除原有
源码构建覆写，或明确指定 Release 包：

```nix
programs.oh-my-ime = {
  enable = true;
  package = inputs.oh-my-ime.packages.${pkgs.stdenv.hostPlatform.system}.default;
  rime.enable = true;
  rime.package = inputs.oh-my-ime.packages.${pkgs.stdenv.hostPlatform.system}.rime;
};
```

使用 profile 安装：

```sh
nix profile install 'https://github.com/Ign1x/oh-my-ime/releases/latest/download/oh-my-ime-nix.tar.gz#full'
```

`default` 包含平台、Echo 和 Sparkle，`rime` 包含 Rime 引擎，`full` 合并两者。
托管 Fcitx5/IBus 仍通过模块配置。如果还经过自己的 `mynixpkgs` 包装仓库，也应将
其中的输入或 overlay 切到 Release；继续引用源码包会继续编译。

## 附件与运行要求

每次发布包括：

- `oh-my-ime-VERSION-SYSTEM.tar.gz`：平台二进制、清单和桌面文件。
- `oh-my-ime-rime-VERSION-SYSTEM.tar.gz`：Rime 引擎和清单。
- `oh-my-ime-nix.tar.gz`：含 NixOS/Home Manager 模块和二进制包的最小 flake。
- `release.json`、`SHA256SUMS`：版本、提交、架构、下载地址和校验值。

发行版打包流程还增加平台与 Rime 的 `.deb`、`.rpm`，每个架构的 `.AppImage` 和
`.AppImage.build.json`，以及包含 `PKGBUILD`、`.SRCINFO` 的 `oh-my-ime-aur-VERSION.tar.gz`。
新标签发布时，主清单和校验和统一覆盖这些格式。生成 AUR 配方不会自动发布到 AUR。

`SYSTEM` 为 `x86_64-linux` 或 `aarch64-linux`。构建环境为 Ubuntu 24.04，要求
glibc ≥ 2.39，以及 Wayland、libxkbcommon、fontconfig、图形和窗口系统运行库。
Rime 还需 librime ≥ 1.11（`librime.so.1`）和方案数据。Nix 包会提供依赖及搜索路径；
其他发行版的手动安装方式见压缩包中的 `README.txt`。这些包不是全静态程序。

## 发布新版本

工作流使用 GitHub 自动提供的临时 `GITHUB_TOKEN`，仅发布任务授予
`contents: write`，无需个人访问令牌。用现有 SSH 凭据推送 tag 即可；其他工作流
用 `GITHUB_TOKEN` 创建的 tag 不会自动触发新的工作流。

1. 更新 `Cargo.toml` 中的 workspace 版本、三个内部依赖版本和 `Cargo.lock`，
   整理 changelog 并提交已验证的源码。
2. 在 **Actions → Release → Run workflow** 选择该提交所在分支，或将该提交推送到
   `release/` 分支，先验证两个架构，不公开 Release。
3. 给该提交打 `vVERSION` tag 并推送；tag 必须与 `Cargo.toml` 版本相同。
4. 工作流验证两个架构，组装并求值 Release flake，将全部附件上传到草稿，再公开
   Release。`-rc.1` 等带后缀的 tag 作为预发布，不替换稳定版 latest。

若存在 `docs/release-notes/vVERSION.md`，工作流会用它作为版本说明；否则由 GitHub
根据合入的改动生成。

流程拒绝覆盖已有 Release。若上传失败留下草稿，检查或删除该草稿后再重试；
已发布二进制发生变化时应使用新版本，因为 Nix 用户会固定其校验值。

本地测试打包（替换 VERSION，使用对应架构的 ELF 程序）：

```sh
python3 scripts/release.py package --tag vVERSION --system x86_64-linux \
  --bin-dir target/release --out /tmp/omime-release
nix build --impure --file nix/check-release.nix \
  --argstr artifacts /tmp/omime-release --out-link result-release
python3 scripts/release.py assemble --tag vVERSION --systems x86_64-linux \
  --out /tmp/omime-release
```

单架构的本地产物用于验证；正式发布要求两个原生构建任务均通过、提交一致且
四个二进制压缩包的校验值全部匹配。
流程还在两个原生 runner 上构建、验证发行版包；生成最终清单和校验和前，
`assemble --linux-packages` 会拒绝缺失格式、混用提交或内容被修改的安装包。
