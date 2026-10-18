//! 节点端点：URL + 固定 node_id（+ 可选的固定节点公钥）。
//!
//! 节点身份以 node_id 为准，URL 只是传输位置：换 URL 不会静默换节点（与 TYPES.md 的
//! “更换 URL 不自动更换节点公钥”一致）。可选公钥固定后，每个挑战都必须由该公钥签名。
use chat_node_services::canonical::{canonical_path, canonical_query, validate_bounded_id};
use chat_node_services::limits::{MAX_NODE_ID_BYTES, MAX_PATH_BYTES, MAX_QUERY_BYTES};
use reqwest::Url;

use crate::error::NodeClientError;

/// 一个被固定的节点端点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEndpoint {
    url: Url,
    node_id: String,
    node_public_key: Option<[u8; 32]>,
}

impl NodeEndpoint {
    /// 用公开 URL 与 node_id 建立端点；URL 只允许 http/https、无凭据、无查询/片段、无子路径。
    ///
    /// “无子路径”与“拒绝重定向”是同一件事的两半：节点路由就是绝对路径，
    /// 一旦允许前缀或跟随 3xx，签名原文（规范 path）与线上路径就可能不一致。
    pub fn new(base_url: &str, node_id: &str) -> Result<Self, NodeClientError> {
        validate_bounded_id("node id", node_id, MAX_NODE_ID_BYTES)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let url = Url::parse(base_url).map_err(|_| NodeClientError::config("endpoint url is not valid"))?;
        match url.scheme() {
            "http" | "https" => {}
            _ => return Err(NodeClientError::config("endpoint url must be http or https")),
        }
        if url.host_str().unwrap_or_default().is_empty() {
            return Err(NodeClientError::config("endpoint url has no host"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(NodeClientError::config("endpoint url must not carry credentials"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(NodeClientError::config("endpoint url must not carry query or fragment"));
        }
        if url.path() != "/" && !url.path().is_empty() {
            return Err(NodeClientError::config("endpoint url must not carry a path prefix"));
        }
        // 统一成“无尾斜杠”，避免同一端点两种字符串形式。
        let mut url = url;
        url.set_path("");
        Ok(Self {
            url,
            node_id: node_id.to_owned(),
            node_public_key: None,
        })
    }

    /// 固定节点签名公钥（推荐）：之后每个挑战都要用该公钥验签，node_id 单独固定不足以
    /// 防“同一 ID 的伪造节点”。
    pub fn with_node_public_key(mut self, node_public_key: [u8; 32]) -> Self {
        self.node_public_key = Some(node_public_key);
        self
    }

    /// 固定节点标识。
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// 固定节点公钥（未固定时为 None）。
    pub fn node_public_key(&self) -> Option<&[u8; 32]> {
        self.node_public_key.as_ref()
    }

    /// 公开 URL 文本（含端口，不含凭据）。
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// 构造一次请求的完整 URL；path/query 先按节点规则规范化，保证“签名原文 == 线上原文”。
    pub fn request_url(&self, path: &str, query: &str) -> Result<Url, NodeClientError> {
        if path.len() > MAX_PATH_BYTES || query.len() > MAX_QUERY_BYTES {
            return Err(NodeClientError::local_invalid("request path or query is too long"));
        }
        let path = canonical_path(path)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let query = canonical_query(query)
            .map_err(|error| NodeClientError::from_local_node_error(&error))?;
        let mut url = self.url.clone();
        url.set_path(&path);
        url.set_query(if query.is_empty() { None } else { Some(&query) });
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalizes_url_and_binds_node_id() {
        let endpoint = NodeEndpoint::new("http://127.0.0.1:8080/", "node-1").expect("endpoint");
        assert_eq!(endpoint.node_id(), "node-1");
        // url crate 对层级 URL 固定输出根路径 "/"；请求 URL 会显式设置完整 path。
        assert_eq!(endpoint.url().as_str(), "http://127.0.0.1:8080/");
        assert!(endpoint.node_public_key().is_none());
        let url = endpoint
            .request_url("/v1/groups/grp:1:aa/head", "")
            .expect("request url");
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/v1/groups/grp:1:aa/head");
        let url = endpoint
            .request_url("/v1/mailbox", "limit=2&cursor=cur1.aa.bb")
            .expect("request url with query");
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:8080/v1/mailbox?limit=2&cursor=cur1.aa.bb"
        );
    }

    #[test]
    fn insecure_or_ambiguous_urls_are_rejected() {
        assert!(NodeEndpoint::new("ftp://127.0.0.1", "node-1").is_err());
        assert!(NodeEndpoint::new("http://user:pw@127.0.0.1", "node-1").is_err());
        assert!(NodeEndpoint::new("http://127.0.0.1/node1", "node-1").is_err());
        assert!(NodeEndpoint::new("http://127.0.0.1?a=1", "node-1").is_err());
        assert!(NodeEndpoint::new("http://127.0.0.1#frag", "node-1").is_err());
        assert!(NodeEndpoint::new("not a url", "node-1").is_err());
        assert!(NodeEndpoint::new("http://127.0.0.1", "node 1").is_err());
        assert!(NodeEndpoint::new("http://127.0.0.1", &"n".repeat(200)).is_err());
    }

    #[test]
    fn request_url_rejects_non_canonical_paths() {
        let endpoint = NodeEndpoint::new("http://127.0.0.1:1", "node-1").expect("endpoint");
        assert!(endpoint.request_url("v1/groups", "").is_err());
        assert!(endpoint.request_url("/v1/../etc", "").is_err());
        assert!(endpoint.request_url("/v1/%2e", "").is_err());
        assert!(endpoint.request_url("/v1/mailbox", "a=1#frag").is_err());
    }

    #[test]
    fn pinned_node_key_is_recorded() {
        let endpoint = NodeEndpoint::new("https://node.example", "node-1")
            .expect("endpoint")
            .with_node_public_key([7_u8; 32]);
        assert_eq!(endpoint.node_public_key(), Some(&[7_u8; 32]));
    }
}
