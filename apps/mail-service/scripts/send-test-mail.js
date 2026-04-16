// 测试发送邮件脚本
// 用法: node send-test-mail.js <playerId>
import http from 'http';

const playerId = process.argv[2] || 'player_001';

const data = JSON.stringify({
  to_player_id: playerId,
  sender_type: 'system',
  sender_id: 'system',
  sender_name: '系统',
  title: '测试邮件',
  content: '这是一封测试邮件',
  attachments: [{ type: 'item', id: 1001, count: 1 }],
  mail_type: 'system',
  created_by_type: 'script',
  created_by_id: 'send-test-mail',
  created_by_name: 'send-test-mail.js'
});

console.log('Sending to:', playerId);

const options = {
  hostname: '127.0.0.1',
  port: 9003,
  path: '/api/v1/mails',
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'Content-Length': Buffer.byteLength(data)
  }
};

const req = http.request(options, (res) => {
  let body = '';
  res.on('data', chunk => body += chunk);
  res.on('end', () => {
    console.log('Response:', body);
  });
});

req.on('error', (e) => {
  console.error('Error:', e.message);
});

req.write(data);
req.end();
