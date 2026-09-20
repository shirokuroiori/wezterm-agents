# wezterm-agents 残作業

段階1（hookのRustサブコマンド化・状態ディレクトリのXDG化）・段階2（Lua
プラグイン抽出）・リポジトリ分離・段階3の一部（`init`/`install` サブコマンド
によるhooks自動設定）までは完了。ここから public 化までの残作業。

## 公開前に必須

- [x] ~~コード全体の最終レビュー~~ → `/code-review high src/ plugin/`（
      `origin/main...HEAD` の単一コミット、22ファイル +1160/-924 が対象）で
      実施。指摘事項ゼロ。コメント以外のコード差分は無し（実行コードは
      全ファイルでバイト単位一致、変わったのはコメント・doc comment・
      assert失敗メッセージの英訳のみ）と確認済み。`cargo test` 47件通過
- [ ] `gh repo edit shirokuroiori/wezterm-agents --visibility public` で公開に戻す
- [x] ~~コードコメントの英訳~~ → src/*.rs（テストファイル含む）・plugin/init.lua・
      plugin/shell-integration/.zshenv の `//`/`///`/`//!`/`--`/`#` コメントを
      全訳。`cargo test` 47件通過を確認済み。エージェント向けのデバッグログ
      文字列・CLI エラーメッセージ・ベル通知本文・`## ログ`/`# メモ` の
      Markdown 見出しリテラル（メモファイルの実データ形式）は意図的に未訳
      のまま残した（挙動保持のため）

## 配布・インストール

- [ ] GitHub Releases のビルドパイプライン（CI、クロスコンパイル: macOS
      arm64/x86_64, Linux x86_64/arm64 など）
- [ ] `install.sh` / README に「cargo が無い環境」向けのフォールバックを追加
      （Releases からビルド済みバイナリを取得）
- [ ] `wezterm.plugin.require 'https://github.com/...'` の実地動作確認。
      **未検証**: `file://` は対象が git リポジトリである必要があり失敗した経緯
      があるが、`https://` 経由の実クローン・`plugin/init.lua` 読み込みは
      一度も試せていない
- [ ] `dofile()` から `wezterm.plugin.require` への切り替え判断（開発が
      落ち着いたタイミングで）。切り替えたら dotfiles 側 `wezterm.lua` も追従

## CI・品質

- [ ] GitHub Actions で `cargo test` / `cargo clippy` / Lua 構文チェック
      （stylua か luacheck）を回す
- [ ] 新規マシンでのインストールフローを通しで確認
      （フレッシュ clone → `install.sh` 相当の手順）

## ドキュメント

- [ ] README の拡充（現状は最低限のみ。使用例・スクリーンショット・
      トラブルシューティングなど）
- [ ] 設計文書（dotfiles 側 `docs/plans/wezterm-multi-agent-spec.md` 相当、
      937行）を英訳した上でこのリポジトリに移植するか検討。実測結果や
      判断理由が詰まった資産なので、削らず活かす方向で
- [ ] `agents.status()` / `agents.apply_to_config()` のAPIリファレンスを
      `plugin/init.lua` のコメントから抜き出し、README か別ファイル
      （例: DESIGN.md）に独立させるか検討

## 機能・設計

- [x] ~~エージェント追加を「アダプタ表」として構造化する~~ →
      `wezterm-agents init <shell>`（Claude Code、`--settings` インライン
      JSON によるシェル関数シム）・`wezterm-agents install copilot`
      （Copilot CLI、ドロップインファイル書き込み）を実装した。
      settings.json を手で書く必要はもう無い。Codex/Gemini/aider 等
      新エージェントの追加はまだ setup.rs に手を入れる必要があるが、
      パターンは確立できた
- [ ] Claude Code hook 注入の bash / fish 対応。2026-09-16 に zsh のみで
      実装（プラグインの ZDOTDIR 注入 + `install claude` による `~/.zshenv`
      追記）。bash は `.bash_profile` + `.bashrc` への追記（実測: `bash -lc`
      は `.bash_profile` と `BASH_ENV` を読む）、fish は
      `$XDG_DATA_DIRS/fish/vendor_conf.d` の自動ロードが使える。設計の比較は
      https://claude.ai/artifact/BJ43kbW4HS7ydx2NhTt3so
- [ ] エディタ / IDE 拡張から直接起動した claude 向けに、`install claude`
      が `~/.claude/settings.json` へ冪等マージするオプション（settings.json は
      唯一シェルを経由しない注入点。Claude Code は hooks を起動時に
      スナップショットする点に注意）
- [ ] `claude_status`/`copilot_status`（旧 user var 名）の互換送信を
      いつ廃止するか
- [ ] `apply_to_config({ tab_title = true })` の既定タブ描画（自前の
      `format-tab-title` を持たない利用者向け）が**実機で未検証**。README で
      「これだけで動く」と案内しているので、実際に確認が必要
- [ ] Linux/Windows での動作確認。`wezterm_bin()` のパス探索・hook の
      バイナリ経路・`util.rs` の `date` コマンド依存などは macOS 中心
- [ ] GitHub Releases 以外の配布経路（crates.io、Homebrew tap 等）を
      追加するか

## 調査済み・不採用（記録用）

- `wezterm cli set-tab-title` へ ANSI エスケープを書き込む方式は技術的に
  可能と実機確認済み（tab_title に truecolor 背景色が出た）が、自前の
  `format-tab-title` を持つ利用者と競合するため不採用。上記の設計文書に
  経緯を残すと良い
