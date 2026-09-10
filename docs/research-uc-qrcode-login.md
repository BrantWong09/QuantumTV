# 调研：UC 网盘扫码登录二维码内容

日期：2026-09-07。仅调研，未写代码。

## 结论

### 1. UC 网盘二维码内容（确认，已读源码，5 个独立仓库一致）

```
https://su.uc.cn/1_n0ZCv?uc_param_str=dsdnfrpfbivesscpgimibtbmnijblauputogpintnwktprchmt&token=<cas_token>&client_id=381&uc_biz_str=S%3Acustom%7CC%3Atitlebar_fix
```

- 域名：`su.uc.cn`（UC 专用短链域，不是 `su.quark.cn`）
- 路径代码：`1_n0ZCv`
- query：`uc_param_str`（固定长串）+ `token` + `client_id=381` + `uc_biz_str=S%3Acustom%7CC%3Atitlebar_fix`（即 `S:custom|C:titlebar_fix` URL 编码）
- **没有** `ssb=weblogin`（那是夸克参数）

来源（均为 raw 源码全文已读）：
- xiaoyaDev/xiaoya-alist `glue_python/uc_cookie/uc_cookie.py:169-170`
  https://github.com/xiaoyaDev/xiaoya-alist/blob/master/glue_python/uc_cookie/uc_cookie.py
- Greatwallcorner/CatVodSpider `src/main/java/com/github/catvod/api/UCApi.java:450`（master 分支）
  https://github.com/Greatwallcorner/CatVodSpider/blob/master/src/main/java/com/github/catvod/api/UCApi.java
- ByLsPro/JxPan `PyTool/UC网盘扫码登录.py` get_qr_url()
  https://github.com/ByLsPro/JxPan/blob/master/PyTool/UC%E7%BD%91%E7%9B%98%E6%89%AB%E7%A0%81%E7%99%BB%E5%BD%95.py
- woleigedouer/cookie-butler `config/platforms.json` uc.qrUrlTemplate
  https://github.com/woleigedouer/cookie-butler/blob/main/config/platforms.json
- kknifer7/CatVodSpider-PC `UCApi.java:450`（grep.app 命中，未逐行读全文，行号与 Greatwallcorner 完全一致，应为同源拷贝）

### 2. 实测失败原因解释

nuu987/tvbox-auxiliary 与 riowang88/tvbox-source-aggregator 的 `src/core/cloud-login.ts` ucHandler 用的正是
`https://su.quark.cn/4_eMHBJ?token=<uc_token>&client_id=381&ssb=weblogin`（nuu987 第 239 行、riowang88 第 288 行，两文件近逐字相同，疑为同一份 AI 生成代码互相拷贝）。该写法与本次实测失败现象一致——**这两处 UC handler 大概率是错的、未经实测**。正确做法应改用上面 `su.uc.cn/1_n0ZCv` 格式。

- https://github.com/nuu987/tvbox-auxiliary/blob/main/src/core/cloud-login.ts
- https://github.com/riowang88/tvbox-source-aggregator/blob/main/src/core/cloud-login.ts

### 3. 扫码 App 要求（确认，多个实现的提示文案）

- UC：必须用 **UC 系 App**（UC浏览器 或 UC网盘 App）扫，各实现提示文案：
  - xiaoya uc_cookie.py：`请打开 UC浏览器 APP 扫描此二维码！`
  - JxPan：`请使用【手机 UC 浏览器】扫描下方二维码`
  - CatVodSpider UCApi.java 对话框标题：`请使用UC网盘app扫描`，通知 `请使用uc网盘App扫描二维码`
- 夸克：各实现均要求 **夸克 App** 内扫（xiaoya quark_cookie.py `请打开 夸克 APP 扫描此二维码！`；fancydirty/mediary-scout 代码注释 "user scans the su.quark.cn URL in the 夸克 App"；nianzhibai/91 `qr.go` 状态文案 "等待使用夸克 App 扫码"）。

### 4. H5 浏览器内能否完成确认？（未验证，但无任何反例）

未找到任何实现在手机浏览器 H5 页面完成 CAS 确认；所有已知实现都要求 App 内扫码+确认。结合本次实测（短链被夸克 App 拦截跳下载页），判断：`su.*` 短链在浏览器打开只会落到下载/推广页，确认动作只能在对应家族 App 内完成。`ssb=weblogin` 参数名暗示这是给 App 的 web-login 握手标记，但该推断**未经官方文档证实**。

## 备忘：夸克侧对照（已读源码）

夸克二维码同样存在参数变体，均为 `su.quark.cn/4_eMHBJ`：
- xiaoya `quark_cookie.py:150`：`token&client_id=532&ssb=weblogin&uc_param_str=&uc_biz_str=S%3Acustom%7COPT%3ASAREA%400%7COPT%3AIMMERSIVE%401%7COPT%3ABACK_BTN_STYLE%400`
- nianzhibai/91 `backend/internal/drives/quark/qr.go` buildQRLoginURL：同上参数（代码拼装）
- fancydirty/mediary-scout：`token&client_id=532&v=1.2&uc_param_str=`（无 ssb 也能跑通，说明 ssb/uc_biz_str 非硬性）

## 轮询与票据兑换（两端同构，已读源码）

| 步骤 | UC | 夸克 |
|---|---|---|
| 取 token | POST `https://api.open.uc.cn/cas/ajax/getTokenForQrcodeLogin` (client_id=381, v=1.2) | GET/POST `https://uop.quark.cn/cas/ajax/getTokenForQrcodeLogin` (client_id=532, v=1.2) |
| 轮询 | `https://api.open.uc.cn/cas/ajax/getServiceTicketByQrcodeToken`，status 2000000=成功 / 50004001=等待 / 50004002=过期 | `https://uop.quark.cn/cas/ajax/getServiceTicketByQrcodeToken`，同一套 status 码 |
| 兑换 cookie | GET `https://drive.uc.cn/account/info?st=<service_ticket>`（xiaoya）；JxPan 另走 `fast.uc.cn/api/info` + `drive.uc.cn/api/v1/sso/callback?ticket=` | GET `https://pan.quark.cn/account/info?st=<st>&lw=scan` |

status 码两端一致：2000000 成功、50004001 等待扫码、50004002 二维码失效。
