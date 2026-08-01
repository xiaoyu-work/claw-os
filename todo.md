# TODO

- [ ] 配置 Claw OS APT 仓库签名密钥
  - 生成专用的 GPG/OpenPGP 签名密钥。
  - 将私钥保存为 GitHub Actions Secret：`CLAW_OS_APT_SIGNING_PRIVATE_KEY`。
  - 私钥有密码时，配置 `CLAW_OS_APT_SIGNING_PASSPHRASE`。
  - 发布对应公钥，供系统验证 APT 仓库签名。
  - 不要将私钥或密码提交到仓库。
