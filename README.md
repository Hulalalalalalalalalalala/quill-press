# QuillPress

内容编辑与刊物发布。

需要Node.js 24 或更新版本。直接运行 TypeScript，不需要安装其他包。

查看命令帮助：

```sh
node server.ts --help
```

启动本地服务：

```sh
node server.ts serve --host 127.0.0.1 --port 8080 --data-dir data
```

打开 http://127.0.0.1:8080 查看首页。Ctrl+C 停止服务。`--data-dir` 指定本地业务数据目录，重启时继续使用同一目录。

接口：

- `GET /health` 返回服务状态和产品名称。
- `GET /api/articles` 返回文章列表，首次启动时为空。
- `POST /api/articles` 保存一篇草稿文章。请求体为 JSON 对象，包含 `title`、`body` 和可省略的 `summary`；成功返回 `201` 和保存后的文章（`status` 为 `draft`）。标题去掉首尾空白后不能为空，`summary` 省略时保存为空字符串。
- 未知路径返回 404，已知路径不支持的方法返回 405，`Allow` 头列出该路径实际支持的方法。

```sh
curl http://127.0.0.1:8080/health
curl http://127.0.0.1:8080/api/articles
curl -X POST http://127.0.0.1:8080/api/articles \
  -H 'content-type: application/json' \
  -d '{"title":"  第一篇  ","summary":"摘要","body":"正文内容"}'
```
