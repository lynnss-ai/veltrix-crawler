// 随机密码生成:大小写字母 + 数字,默认 10 位(>6),去除易混淆字符 0/O/1/I/l。
const PASSWORD_CHARS =
  "ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
const DEFAULT_PASSWORD_LENGTH = 10;

export function generatePassword(length = DEFAULT_PASSWORD_LENGTH): string {
  let result = "";
  for (let i = 0; i < length; i += 1) {
    result += PASSWORD_CHARS[Math.floor(Math.random() * PASSWORD_CHARS.length)];
  }
  return result;
}
