//! Turns the safe Markdown tree into Yew nodes. Text always goes through Yew's escaping.
use wasm_bindgen::JsValue;
use yew::prelude::*;

use crate::markdown::{Align, Node, parse};

#[derive(Properties, PartialEq)]
pub struct MarkdownProps {
    pub source: AttrValue,
}

#[function_component(Markdown)]
pub fn markdown(props: &MarkdownProps) -> Html {
    let nodes = use_memo(props.source.clone(), |s| parse(s));
    html! { <div class="md">{ blocks(&nodes) }</div> }
}

fn blocks(nodes: &[Node]) -> Html {
    nodes.iter().map(node).collect::<Html>()
}

fn align_class(a: Align) -> Option<&'static str> {
    match a {
        Align::None => None,
        Align::Left => Some("al-left"),
        Align::Center => Some("al-center"),
        Align::Right => Some("al-right"),
    }
}

fn node(n: &Node) -> Html {
    match n {
        Node::Text(t) => html! { { t.clone() } },
        Node::Code(t) => html! { <code>{ t.clone() }</code> },
        Node::Em(c) => html! { <em>{ blocks(c) }</em> },
        Node::Strong(c) => html! { <strong>{ blocks(c) }</strong> },
        Node::Strike(c) => html! { <del>{ blocks(c) }</del> },
        // Links are shown, not followed: inside the app a click must never navigate the window.
        Node::Link { url, children } => match url {
            Some(url) => html! { <span class="md-link" title={url.clone()}>{ blocks(children) }</span> },
            None => html! { <span>{ blocks(children) }</span> },
        },
        Node::Break => html! { <br /> },
        Node::Rule => html! { <hr /> },
        Node::Paragraph(c) => html! { <p>{ blocks(c) }</p> },
        Node::Heading { level, children } => match level {
            1 => html! { <h1>{ blocks(children) }</h1> },
            2 => html! { <h2>{ blocks(children) }</h2> },
            3 => html! { <h3>{ blocks(children) }</h3> },
            4 => html! { <h4>{ blocks(children) }</h4> },
            5 => html! { <h5>{ blocks(children) }</h5> },
            _ => html! { <h6>{ blocks(children) }</h6> },
        },
        Node::CodeBlock { lang, text } => html! { <CodeBlock lang={lang.clone()} text={text.clone()} /> },
        Node::List { start, items } => {
            let rows = items.iter().map(|item| {
                let task = matches!(item.first(), Some(Node::Task(_)));
                html! { <li class={task.then_some("task")}>{ blocks(item) }</li> }
            });
            match start {
                Some(1) => html! { <ol>{ for rows }</ol> },
                Some(n) => html! { <ol start={n.to_string()}>{ for rows }</ol> },
                None => html! { <ul>{ for rows }</ul> },
            }
        }
        Node::Task(done) => html! { <input type="checkbox" checked={*done} disabled=true aria-label={if *done { "done" } else { "not done" }} /> },
        Node::Quote(c) => html! { <blockquote>{ blocks(c) }</blockquote> },
        Node::Table { aligns, head, rows } => {
            let cls = |i: usize| aligns.get(i).copied().and_then(align_class);
            html! {
                <div class="md-table"><table>
                    <thead><tr>{ for head.iter().enumerate().map(|(i, c)| html! { <th class={cls(i)}>{ blocks(c) }</th> }) }</tr></thead>
                    <tbody>
                        { for rows.iter().map(|r| html! { <tr>{ for r.iter().enumerate().map(|(i, c)| html! { <td class={cls(i)}>{ blocks(c) }</td> }) }</tr> }) }
                    </tbody>
                </table></div>
            }
        }
        // Table scaffolding is consumed by the parser.
        Node::Cell(_) | Node::Row(_) | Node::HeadRow(_) => html! {},
    }
}

#[derive(Properties, PartialEq)]
struct CodeBlockProps {
    lang: Option<String>,
    text: String,
}

fn copy_to_clipboard(text: &str) {
    let global = js_sys::global();
    let Ok(nav) = js_sys::Reflect::get(&global, &JsValue::from_str("navigator")) else { return };
    let Ok(clip) = js_sys::Reflect::get(&nav, &JsValue::from_str("clipboard")) else { return };
    if clip.is_undefined() {
        return;
    }
    if let Ok(write) = js_sys::Reflect::get(&clip, &JsValue::from_str("writeText")) {
        if let Ok(f) = write.dyn_into::<js_sys::Function>() {
            let _ = f.call1(&clip, &JsValue::from_str(text));
        }
    }
}

use wasm_bindgen::JsCast;

#[function_component(CodeBlock)]
fn code_block(props: &CodeBlockProps) -> Html {
    let copied = use_state(|| false);
    let onclick = {
        let text = props.text.clone();
        let copied = copied.clone();
        Callback::from(move |_| {
            copy_to_clipboard(&text);
            copied.set(true);
        })
    };
    html! {
        <div class="md-code">
            <div class="md-code-head">
                <span>{ props.lang.clone().unwrap_or_default() }</span>
                <button class="small" {onclick}>{ if *copied { "Copied" } else { "Copy" } }</button>
            </div>
            <pre><code>{ props.text.clone() }</code></pre>
        </div>
    }
}
