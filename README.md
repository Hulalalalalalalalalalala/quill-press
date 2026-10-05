# chainview

chainview 是一个查看 X.509 证书信息的命令行小工具。当前版本提供：

- `chainview --version`：输出版本号；
- `chainview inspect <文件路径>`：查看一张本地 DER 编码 X.509 证书的基本字段。

## 构建与运行

```sh
cargo build --offline
./target/debug/chainview --version
```

输出：

```text
chainview 0.1.0
```

## 查看证书

```sh
./target/debug/chainview inspect cert.der
```

其中 `cert.der` 必须是 **DER 编码**的 X.509 证书文件（不支持 PEM 文本、
PKCS#7/PEM 证书集合，也不做签名验证）。文件必须恰好包含一张完整证书，
证书数据之后附有任何字节（包括空白、换行或另一张证书）都会被拒绝。

成功时证书信息输出到**标准输出**，每个字段单独一行，顺序固定：

```text
Subject: CN=example.com,O=示例公司,C=CN
Issuer: CN=Example CA,O=示例公司,C=CN
Serial Number: 0E8A4C2F9B17D603
Not Before: 2026-01-15T09:30:00Z
Not After: 2027-01-15T09:30:00Z
```

字段含义：

| 字段 | 含义 |
| --- | --- |
| `Subject` | 证书主体的完整可辨别名称（DN，RFC 4514 格式），保留全部名称属性及重复出现的属性；多值 RDN 以 `+` 连接。没有主体名称的合法证书该字段显示为空。 |
| `Issuer` | 颁发者的完整可辨别名称，格式与 `Subject` 相同。 |
| `Serial Number` | 证书序列号，以大写十六进制表示（不含 DER 整数的符号填充字节）。 |
| `Not Before` | 有效期开始时间，统一转换为 UTC，格式为 `YYYY-MM-DDTHH:MM:SSZ`。 |
| `Not After` | 有效期结束时间，格式同上。 |

名称中的非 ASCII 内容（例如中文）按 UTF-8 原样显示，不会被转义或替换。
时间始终按 UTC 输出，不受运行机器所在时区影响。

属性类型没有已知短名称（如 `CN`、`O`）时，按 RFC 4514 用点分十进制
OID 标识该属性，等号之后以 `#` 引出该属性值完整 DER 编码（标签、长度
和内容）的大写十六进制表示。这里的 `#` 是编码标志，不是属性原本包含的
文字，因此不加转义反斜杠；即使值恰好是可读文本也一律按编码显示。例如
类型 `1.2.3.4` 的值是 UTF8String `abc` 时显示为
`1.2.3.4=#0C03616263`。同一名称中已知与未知属性可以混用、重复出现，
各自按自身类型决定表示形式。

`inspect` 只展示解码得到的字段：它不验证证书签名，也不判断证书是否受信任；
即使证书尚未生效或已经过期，仍然可以正常查看。

### 失败与退出状态

- 成功：退出码 `0`，证书信息写入标准输出。
- 用法错误（缺少文件路径、提供多个路径、未知参数等）：退出码 `2`，
  标准错误输出正确用法：

  ```text
  Usage: chainview inspect <DER-FILE>
         chainview --version
  ```

- 文件不存在或无法读取（权限不足、路径是目录等）：退出码 `2`，
  标准错误输出 `chainview: cannot read certificate file '<路径>': <原因>`。
- 证书格式不正确（空文件、截断或损坏的数据、PEM 或其他 ASN.1 对象、
  证书后附带多余字节等）：退出码 `2`，标准错误输出
  `chainview: invalid DER certificate: <原因>`。

所有失败情况下标准输出保持为空，原因只写入标准错误。
