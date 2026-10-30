# 配置原子事务

`SqliteRepository::migrate()` 将旧数据库幂等升级到版本 3。新增的
`configuration_revision` 表只保存配置版本号，初始值为 0；现有设置、终端配置、
连接目录、导入标记和凭据日志仍由原来的表及接口负责。

## 公共接口

- `load_configuration() -> Result<ConfigurationSnapshot, StorageError>`：
  在同一个读事务中返回 `revision`、完整 `AppSettings` 及全部 `TerminalProfile`。
  不应将旧的多个独立读取接口拼成带版本号的快照。
- `commit_configuration(ConfigurationChange) -> Result<ConfigurationCommitOutcome, StorageError>`：
  一个 worker 请求进入一个 SQLite `IMMEDIATE` 事务，首先比较 `expected_revision`，
  再保存全部修改。`settings` 是完整替换；`upsert_profiles` 按 ID 创建或更新；
  `delete_profiles` 按 ID 删除。未列出的终端配置不变。
- 成功返回 `Applied { revision }`；每次成功提交（即使内容相同）递增一次版本号。
  版本失配返回 `Conflict { actual_revision }`，不写入任何数据。
  调用方需重新读取、合并或让用户确认，不应自动用新版本号重放旧修改。
- 无效设置、重复 ID、保存与删除列表交叉、删除不存在的配置、删除提交前默认配置、
  删除仍被连接引用的配置均返回 `StorageError::Constraint`。同一提交不能先切换默认
  再删除原默认配置；必须先成功切换默认，再以新版本单独删除。
  新默认配置必须存在于提交后的配置集合中。
- 旧的 `save_settings` 和 `save_terminal_profile` 签名及单项保存行为保留；
  它们也在同一写事务内递增版本号，从而使先前快照失效。

新提交使用现有 core 校验器验证设置及待保存配置，不重写或清理未修改的历史配置。
版本号达到 SQLite 有符号整数上限后拒绝进一步写入，不回绕。
任一事务内 SQL、序列化、校验或提交失败都会回滚整批修改，包括版本号。
和其他存储接口一样，若进程退出或 worker 通道在提交结果送达前中断，调用方应重新
打开并读取持久状态以确认结果，不能仅凭通道错误断言事务未提交。

## 范围与兼容边界

版本号覆盖 core 的完整 `AppSettings` 和 `TerminalProfile`，不覆盖连接目录。
目录引用的安全性由同一 SQLite 写锁与既有 `ON DELETE RESTRICT` 外键保证；
事务不修改连接引用、不级联删除、不操作长期凭据、known-host 或私钥数据。
GuoSSHell 独立偏好及界面接入不在此接口内。

所有并行配置写入方都应使用升级后的存储接口。直接写 SQL 或仍使用旧二进制的
外部写入方不会参与版本协议，不能与新接口混用以获得乐观并发保证。

回归测试使用生产公共接口覆盖内存和文件数据库的并发冲突、组合提交、失败回滚、
重开持久化、默认与目录引用保护、旧接口兼容、历史迁移和版本上限。
