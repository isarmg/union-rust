# 07. Web 前端

前端从编译 catalog 决定导航，未选择模块不出现。生产前端由 union-builder 与后端同 profile
构建并放入 `share/union/web`；不能手工混用其他 revision 的 `dist`。

开发可运行 Vite，正式公网仍只有 Union origin。模块页面使用固定 `/modules/<id>` 前缀。
